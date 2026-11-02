use std::{io::IoSlice, ops::Range};

use rusplay_protocol::{FecHeader, PacketHeader};

use crate::{
    NetError,
    session::{SEALED_HEADROOM, SEALED_TAILROOM, SessionCipher, requires_sealing},
};

const BODY_START: usize = PacketHeader::SIZE + SEALED_HEADROOM;
const NO_SLICE: &[u8] = &[];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Seal {
    Unsealed,
    Pending,
    Sealed,
}

#[derive(Debug, Clone, Copy)]
struct Slot {
    start: usize,
    body_len: usize,
    seal: Seal,
}

impl Slot {
    const fn slot_len(&self) -> usize {
        BODY_START + self.body_len + SEALED_TAILROOM
    }

    const fn plain_len(&self) -> usize {
        PacketHeader::SIZE + self.body_len
    }
}

#[derive(Debug, Default)]
pub struct DatagramArena {
    bytes: Vec<u8>,
    used: usize,
    slots: Vec<Slot>,
}

impl DatagramArena {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self) {
        self.used = 0;
        self.slots.clear();
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    pub fn plain_len(&self, index: usize) -> usize {
        self.slots[index].plain_len()
    }

    pub fn plain_bytes(&self, range: Range<usize>) -> usize {
        self.slots[range].iter().map(Slot::plain_len).sum()
    }

    pub fn plain(&self, index: usize) -> (&[u8], &[u8]) {
        let slot = &self.slots[index];
        let bytes = &self.bytes[slot.start..slot.start + slot.slot_len()];

        (
            &bytes[..PacketHeader::SIZE],
            &bytes[BODY_START..BODY_START + slot.body_len],
        )
    }

    pub fn to_vecs(&self) -> Vec<Vec<u8>> {
        (0..self.len())
            .map(|index| {
                let (header, body) = self.plain(index);

                [header, body].concat()
            })
            .collect()
    }

    pub fn push(
        &mut self,
        header: &PacketHeader,
        fec_header: Option<&FecHeader>,
        payload_len: usize,
    ) -> Result<&mut [u8], NetError> {
        let fec_len = fec_header.map_or(0, |_| FecHeader::SIZE);
        let slot = Slot {
            start: self.used,
            body_len: fec_len + payload_len,
            seal: if requires_sealing(header.stream) {
                Seal::Pending
            } else {
                Seal::Unsealed
            },
        };

        let end = slot.start + slot.slot_len();
        if self.bytes.len() < end {
            self.bytes.resize(end, 0);
        }

        let bytes = &mut self.bytes[slot.start..end];
        header.write(bytes)?;
        if let Some(fec_header) = fec_header {
            fec_header.write(&mut bytes[BODY_START..])?;
        }

        self.used = end;
        self.slots.push(slot);

        Ok(&mut self.bytes[slot.start + BODY_START + fec_len..end - SEALED_TAILROOM])
    }

    pub(crate) fn seal(
        &mut self,
        range: Range<usize>,
        cipher: &SessionCipher,
    ) -> Result<(), NetError> {
        for slot in &mut self.slots[range] {
            if slot.seal != Seal::Pending {
                continue;
            }

            let end = slot.start + slot.slot_len();
            cipher.seal_in_place(&mut self.bytes[slot.start..end])?;
            slot.seal = Seal::Sealed;
        }

        Ok(())
    }

    pub(crate) fn wire(&self, index: usize) -> [IoSlice<'_>; 2] {
        let slot = &self.slots[index];

        match slot.seal {
            Seal::Sealed => [
                IoSlice::new(&self.bytes[slot.start..slot.start + slot.slot_len()]),
                IoSlice::new(NO_SLICE),
            ],
            Seal::Unsealed | Seal::Pending => {
                let (header, body) = self.plain(index);

                [IoSlice::new(header), IoSlice::new(body)]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use rusplay_protocol::{PacketFlags, ProtocolVersion, StreamKind};

    use super::*;
    use crate::{SEALED_OVERHEAD, SessionKeys};

    fn header(stream: StreamKind, sequence: u32) -> PacketHeader {
        PacketHeader {
            version: ProtocolVersion::CURRENT,
            stream,
            flags: PacketFlags::empty(),
            sequence,
            frame: 4,
            fragment_index: 0,
            fragment_count: 1,
        }
    }

    fn ciphers() -> (SessionCipher, SessionCipher) {
        (
            SessionCipher::new(&SessionKeys::new([1; 32], [2; 32])),
            SessionCipher::new(&SessionKeys::new([2; 32], [1; 32])),
        )
    }

    fn joined(arena: &DatagramArena, index: usize) -> Vec<u8> {
        arena
            .wire(index)
            .iter()
            .flat_map(|slice| slice.iter().copied())
            .collect()
    }

    fn fill(arena: &mut DatagramArena, stream: StreamKind, sequence: u32, payload: &[u8]) {
        arena
            .push(&header(stream, sequence), None, payload.len())
            .unwrap()
            .copy_from_slice(payload);
    }

    #[test]
    fn an_unsealed_datagram_is_its_header_followed_by_its_payload() {
        let mut arena = DatagramArena::new();

        fill(&mut arena, StreamKind::Video, 7, b"hello");

        let mut expected = vec![0; PacketHeader::SIZE];
        header(StreamKind::Video, 7).write(&mut expected).unwrap();
        expected.extend_from_slice(b"hello");
        assert_eq!(joined(&arena, 0), expected);
        assert_eq!(arena.to_vecs(), vec![expected]);
        assert_eq!(arena.plain_len(0), PacketHeader::SIZE + 5);
    }

    #[test]
    fn a_fec_header_precedes_the_payload() {
        let fec = FecHeader {
            protects: StreamKind::Video,
            first_fragment: 0,
            data_shards: 2,
            parity_shards: 1,
            parity_index: 0,
            frame_bytes: 99,
        };
        let mut arena = DatagramArena::new();

        arena
            .push(&header(StreamKind::Fec, 1), Some(&fec), 3)
            .unwrap()
            .copy_from_slice(&[1, 2, 3]);

        let datagram = arena.to_vecs().remove(0);

        let (parsed, body) = PacketHeader::parse(&datagram).unwrap();
        let (fec_parsed, payload) = FecHeader::parse(body).unwrap();
        assert_eq!(parsed.stream, StreamKind::Fec);
        assert_eq!(fec_parsed, fec);
        assert_eq!(payload, [1, 2, 3]);
    }

    #[test]
    fn sealing_in_place_round_trips_through_open() {
        let (client, host) = ciphers();
        let mut arena = DatagramArena::new();
        fill(&mut arena, StreamKind::Video, 0, b"first payload");
        fill(&mut arena, StreamKind::Video, 1, &[0xab; 300]);
        let plain = arena.to_vecs();

        arena.seal(0..2, &client).unwrap();

        for (index, expected) in plain.iter().enumerate() {
            let wire = joined(&arena, index);
            assert_eq!(wire.len(), expected.len() + SEALED_OVERHEAD);
            assert_eq!(&wire[..PacketHeader::SIZE], &expected[..PacketHeader::SIZE]);
            assert_ne!(
                &wire[PacketHeader::SIZE + SEALED_HEADROOM..][..4],
                &expected[PacketHeader::SIZE..][..4]
            );
            assert_eq!(&host.open(&wire).unwrap(), expected);
        }
    }

    #[test]
    fn in_place_sealing_matches_the_copying_seal() {
        let (client, host) = ciphers();
        let mut arena = DatagramArena::new();
        fill(&mut arena, StreamKind::Audio, 3, b"same bytes");
        let plain = arena.to_vecs().remove(0);

        arena.seal(0..1, &client).unwrap();
        let in_place = joined(&arena, 0);
        let copied = ciphers().0.seal(&plain).unwrap();

        assert_eq!(in_place, copied);
        assert_eq!(host.open(&in_place).unwrap(), plain);
    }

    #[test]
    fn a_datagram_is_sealed_once_however_often_the_range_is_sealed() {
        let (client, host) = ciphers();
        let mut arena = DatagramArena::new();
        fill(&mut arena, StreamKind::Video, 0, b"once");
        let plain = arena.to_vecs().remove(0);

        arena.seal(0..1, &client).unwrap();
        let first = joined(&arena, 0);
        arena.seal(0..1, &client).unwrap();

        assert_eq!(joined(&arena, 0), first);
        assert_eq!(host.open(&first).unwrap(), plain);
    }

    #[test]
    fn streams_that_do_not_require_sealing_stay_plain() {
        let (client, _) = ciphers();
        let mut arena = DatagramArena::new();
        fill(&mut arena, StreamKind::Pairing, 0, b"handshake");
        let plain = arena.to_vecs().remove(0);

        arena.seal(0..1, &client).unwrap();

        assert_eq!(joined(&arena, 0), plain);
    }

    #[test]
    fn only_the_requested_range_is_sealed() {
        let (client, _) = ciphers();
        let mut arena = DatagramArena::new();
        fill(&mut arena, StreamKind::Video, 0, b"left alone");
        fill(&mut arena, StreamKind::Video, 1, b"sealed");
        let plain = arena.to_vecs();

        arena.seal(1..2, &client).unwrap();

        assert_eq!(joined(&arena, 0), plain[0]);
        assert_eq!(joined(&arena, 1).len(), plain[1].len() + SEALED_OVERHEAD);
    }

    #[test]
    fn a_cleared_arena_reuses_its_storage_without_leaking_old_bytes() {
        let (client, host) = ciphers();
        let mut arena = DatagramArena::new();
        fill(&mut arena, StreamKind::Video, 0, &[0xee; 500]);
        arena.seal(0..1, &client).unwrap();
        let capacity = arena.bytes.len();

        arena.clear();
        fill(&mut arena, StreamKind::Video, 1, b"short");
        let plain = arena.to_vecs();
        arena.seal(0..1, &client).unwrap();

        assert_eq!(arena.len(), 1);
        assert_eq!(arena.bytes.len(), capacity);
        assert_eq!(plain[0].len(), PacketHeader::SIZE + 5);
        assert_eq!(host.open(&joined(&arena, 0)).unwrap(), plain[0]);
    }

    #[test]
    fn plain_bytes_sums_a_range() {
        let mut arena = DatagramArena::new();
        fill(&mut arena, StreamKind::Video, 0, &[0; 10]);
        fill(&mut arena, StreamKind::Video, 1, &[0; 20]);
        fill(&mut arena, StreamKind::Video, 2, &[0; 30]);

        assert_eq!(arena.plain_bytes(1..3), 2 * PacketHeader::SIZE + 50);
        assert_eq!(arena.plain_bytes(0..0), 0);
    }
}
