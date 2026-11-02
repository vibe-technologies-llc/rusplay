use std::borrow::Cow;

use rusplay_protocol::{FecHeader, PacketFlags, PacketHeader, ProtocolVersion, StreamKind};

use crate::{FecScheme, NetError};

#[derive(Debug, Clone)]
pub struct Packetizer {
    stream: StreamKind,
    shard_len: usize,
    max_fragments: u16,
    scheme: FecScheme,
    next_sequence: u32,
}

impl Packetizer {
    pub fn new(
        stream: StreamKind,
        max_datagram: usize,
        scheme: FecScheme,
    ) -> Result<Self, NetError> {
        if matches!(stream, StreamKind::Fec | StreamKind::Control) {
            return Err(NetError::UnfragmentableStream(stream));
        }

        let shard_len = shard_len_for(max_datagram);
        if shard_len == 0 {
            return Err(NetError::DatagramBudgetTooSmall { max_datagram });
        }

        Ok(Self {
            stream,
            shard_len,
            max_fragments: u16::MAX,
            scheme,
            next_sequence: 0,
        })
    }

    #[must_use]
    pub const fn with_max_fragments(mut self, max_fragments: u16) -> Self {
        self.max_fragments = max_fragments;
        self
    }

    pub const fn shard_len(&self) -> usize {
        self.shard_len
    }

    pub const fn next_sequence(&self) -> u32 {
        self.next_sequence
    }

    pub fn max_frame_bytes(&self) -> usize {
        self.shard_len * usize::from(self.max_fragments)
    }

    pub fn packetize(
        &mut self,
        frame: u32,
        keyframe: bool,
        data: &[u8],
    ) -> Result<Vec<Vec<u8>>, NetError> {
        self.packetize_prefixed(frame, keyframe, &[], data)
    }

    pub fn packetize_prefixed(
        &mut self,
        frame: u32,
        keyframe: bool,
        prefix: &[u8],
        body: &[u8],
    ) -> Result<Vec<Vec<u8>>, NetError> {
        let data = Payload { prefix, body };
        if data.is_empty() {
            return Err(NetError::EmptyFrame);
        }

        let fragment_count = u16::try_from(data.len().div_ceil(self.shard_len))
            .ok()
            .filter(|count| *count <= self.max_fragments)
            .ok_or_else(|| NetError::FrameTooLarge {
                bytes: data.len(),
                max_bytes: self.max_frame_bytes(),
            })?;
        let frame_bytes = u32::try_from(data.len()).map_err(|_| NetError::FrameTooLarge {
            bytes: data.len(),
            max_bytes: self.max_frame_bytes(),
        })?;

        let base_flags = if keyframe {
            PacketFlags::KEYFRAME
        } else {
            PacketFlags::empty()
        };

        let blocks = self.scheme.blocks(fragment_count);
        let parity_count: usize = blocks
            .iter()
            .map(|block| usize::from(block.parity_shards))
            .sum();
        let mut datagrams = Vec::with_capacity(usize::from(fragment_count) + parity_count);

        for index in 0..usize::from(fragment_count) {
            let chunk = data.shard(index, self.shard_len);
            let flags = if index + 1 == usize::from(fragment_count) {
                base_flags.with(PacketFlags::LAST_FRAGMENT)
            } else {
                base_flags
            };
            let header = self.next_header(self.stream, flags, frame, index, fragment_count);

            datagrams.push(build_datagram(&header, None, &chunk)?);
        }

        let fec_packet_count =
            u16::try_from(parity_count).map_err(|_| NetError::FrameTooLarge {
                bytes: data.len(),
                max_bytes: self.max_frame_bytes(),
            })?;
        let mut fec_index = 0usize;

        for block in &blocks {
            let first = usize::from(block.first_fragment);
            let shards: Vec<Cow<'_, [u8]>> = (first..first + usize::from(block.data_shards))
                .map(|index| data.padded_shard(index, self.shard_len))
                .collect();
            let shard_refs: Vec<&[u8]> = shards.iter().map(AsRef::as_ref).collect();
            let parity = block.encode(&shard_refs)?;

            for (parity_index, shard) in parity.iter().enumerate() {
                let header = self.next_header(
                    StreamKind::Fec,
                    base_flags,
                    frame,
                    fec_index,
                    fec_packet_count,
                );
                let fec_header = FecHeader {
                    protects: self.stream,
                    first_fragment: block.first_fragment,
                    data_shards: block.data_shards,
                    parity_shards: block.parity_shards,
                    parity_index: u8::try_from(parity_index).unwrap_or(u8::MAX),
                    frame_bytes,
                };

                datagrams.push(build_datagram(&header, Some(&fec_header), shard)?);
                fec_index += 1;
            }
        }

        Ok(datagrams)
    }

    fn next_header(
        &mut self,
        stream: StreamKind,
        flags: PacketFlags,
        frame: u32,
        index: usize,
        count: u16,
    ) -> PacketHeader {
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.wrapping_add(1);

        PacketHeader {
            version: ProtocolVersion::CURRENT,
            stream,
            flags,
            sequence,
            frame,
            fragment_index: u16::try_from(index).unwrap_or(u16::MAX),
            fragment_count: count,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Payload<'a> {
    prefix: &'a [u8],
    body: &'a [u8],
}

impl<'a> Payload<'a> {
    fn len(&self) -> usize {
        self.prefix.len() + self.body.len()
    }

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn range(&self, start: usize, end: usize) -> Cow<'a, [u8]> {
        let split = self.prefix.len();

        if end <= split {
            Cow::Borrowed(&self.prefix[start..end])
        } else if start >= split {
            Cow::Borrowed(&self.body[start - split..end - split])
        } else {
            let mut joined = Vec::with_capacity(end - start);
            joined.extend_from_slice(&self.prefix[start..]);
            joined.extend_from_slice(&self.body[..end - split]);
            Cow::Owned(joined)
        }
    }

    fn shard(&self, index: usize, shard_len: usize) -> Cow<'a, [u8]> {
        let start = index * shard_len;
        let end = (start + shard_len).min(self.len());

        self.range(start, end)
    }

    fn padded_shard(&self, index: usize, shard_len: usize) -> Cow<'a, [u8]> {
        let mut shard = self.shard(index, shard_len);
        if shard.len() < shard_len {
            shard.to_mut().resize(shard_len, 0);
        }

        shard
    }
}

pub const fn shard_len_for(max_datagram: usize) -> usize {
    max_datagram.saturating_sub(PacketHeader::SIZE + FecHeader::SIZE) & !1
}

fn build_datagram(
    header: &PacketHeader,
    fec_header: Option<&FecHeader>,
    payload: &[u8],
) -> Result<Vec<u8>, NetError> {
    let fec_len = if fec_header.is_some() {
        FecHeader::SIZE
    } else {
        0
    };
    let header_len = PacketHeader::SIZE + fec_len;
    let mut datagram = Vec::with_capacity(header_len + payload.len());
    datagram.resize(header_len, 0);

    header.write(&mut datagram)?;
    if let Some(fec_header) = fec_header {
        fec_header.write(&mut datagram[PacketHeader::SIZE..])?;
    }
    datagram.extend_from_slice(payload);

    Ok(datagram)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAX_DATAGRAM: usize = 100;

    fn video(scheme: FecScheme) -> Packetizer {
        Packetizer::new(StreamKind::Video, MAX_DATAGRAM, scheme).unwrap()
    }

    fn headers(datagrams: &[Vec<u8>]) -> Vec<PacketHeader> {
        datagrams
            .iter()
            .map(|datagram| PacketHeader::read(datagram).unwrap())
            .collect()
    }

    #[test]
    fn shard_length_is_even_and_leaves_room_for_both_headers() {
        assert_eq!(shard_len_for(100), 74);
        assert_eq!(shard_len_for(101), 74);
        assert_eq!(shard_len_for(PacketHeader::SIZE + FecHeader::SIZE), 0);
        assert_eq!(shard_len_for(3), 0);
    }

    #[test]
    fn tiny_budget_is_rejected() {
        assert!(matches!(
            Packetizer::new(StreamKind::Video, 20, FecScheme::None),
            Err(NetError::DatagramBudgetTooSmall { max_datagram: 20 })
        ));
    }

    #[test]
    fn control_and_fec_streams_cannot_carry_frames() {
        for stream in [StreamKind::Control, StreamKind::Fec] {
            assert!(matches!(
                Packetizer::new(stream, MAX_DATAGRAM, FecScheme::None),
                Err(NetError::UnfragmentableStream(found)) if found == stream
            ));
        }
    }

    #[test]
    fn empty_frame_is_rejected() {
        assert!(matches!(
            video(FecScheme::None).packetize(0, false, &[]),
            Err(NetError::EmptyFrame)
        ));
    }

    #[test]
    fn the_configured_fragment_cap_bounds_the_frame_size() {
        let mut packetizer = video(FecScheme::None).with_max_fragments(4);
        let shard_len = packetizer.shard_len();

        assert_eq!(packetizer.max_frame_bytes(), shard_len * 4);
        assert!(
            packetizer
                .packetize(0, false, &vec![0; shard_len * 4])
                .is_ok()
        );
        assert!(matches!(
            packetizer.packetize(1, false, &vec![0; shard_len * 4 + 1]),
            Err(NetError::FrameTooLarge { max_bytes, .. }) if max_bytes == shard_len * 4
        ));
    }

    #[test]
    fn oversized_frame_is_rejected() {
        let mut packetizer = video(FecScheme::None);
        let data = vec![0; packetizer.max_frame_bytes() + 1];

        assert!(matches!(
            packetizer.packetize(0, false, &data),
            Err(NetError::FrameTooLarge { .. })
        ));
    }

    #[test]
    fn frame_splits_into_full_fragments_and_a_short_tail() {
        let mut packetizer = video(FecScheme::None);
        let shard_len = packetizer.shard_len();
        let data = vec![7u8; shard_len * 2 + 5];

        let datagrams = packetizer.packetize(9, true, &data).unwrap();
        let headers = headers(&datagrams);

        assert_eq!(datagrams.len(), 3);
        assert_eq!(datagrams[0].len(), PacketHeader::SIZE + shard_len);
        assert_eq!(datagrams[2].len(), PacketHeader::SIZE + 5);
        for (index, header) in headers.iter().enumerate() {
            assert_eq!(usize::from(header.fragment_index), index);
            assert_eq!(header.fragment_count, 3);
            assert_eq!(header.frame, 9);
            assert_eq!(header.sequence, u32::try_from(index).unwrap());
            assert!(header.flags.contains(PacketFlags::KEYFRAME));
            assert_eq!(
                header.flags.contains(PacketFlags::LAST_FRAGMENT),
                index == 2
            );
        }
    }

    #[test]
    fn no_datagram_exceeds_the_budget() {
        let mut packetizer = video(FecScheme::for_overhead(8, 50));
        let data = vec![1u8; 5000];

        let datagrams = packetizer.packetize(1, false, &data).unwrap();

        assert!(
            datagrams
                .iter()
                .all(|datagram| datagram.len() <= MAX_DATAGRAM)
        );
    }

    #[test]
    fn parity_packets_describe_their_block() {
        let mut packetizer = video(FecScheme::for_overhead(4, 50));
        let shard_len = packetizer.shard_len();
        let data = vec![3u8; shard_len * 6];

        let datagrams = packetizer.packetize(5, false, &data).unwrap();
        let fec: Vec<_> = datagrams
            .iter()
            .filter_map(|datagram| {
                let (header, payload) = PacketHeader::parse(datagram).unwrap();
                (header.stream == StreamKind::Fec)
                    .then(|| (header, FecHeader::parse(payload).unwrap()))
            })
            .collect();

        assert_eq!(datagrams.len(), 6 + 2 + 2);
        assert_eq!(fec.len(), 4);
        for (index, (header, (fec_header, parity))) in fec.iter().enumerate() {
            assert_eq!(usize::from(header.fragment_index), index);
            assert_eq!(header.fragment_count, 4);
            assert_eq!(fec_header.protects, StreamKind::Video);
            assert_eq!(fec_header.data_shards, 3);
            assert_eq!(fec_header.parity_shards, 2);
            assert_eq!(fec_header.frame_bytes as usize, data.len());
            assert_eq!(parity.len(), shard_len);
        }
        assert_eq!(fec[0].1.0.first_fragment, 0);
        assert_eq!(fec[2].1.0.first_fragment, 3);
    }

    #[test]
    fn sequence_wraps_across_frames() {
        let mut packetizer = video(FecScheme::None);
        packetizer.next_sequence = u32::MAX;

        let datagrams = packetizer.packetize(0, false, &[1, 2, 3]).unwrap();
        let more = packetizer.packetize(1, false, &[1, 2, 3]).unwrap();

        assert_eq!(headers(&datagrams)[0].sequence, u32::MAX);
        assert_eq!(headers(&more)[0].sequence, 0);
        assert_eq!(packetizer.next_sequence(), 1);
    }

    #[test]
    fn a_prefixed_frame_packetizes_exactly_like_the_joined_frame() {
        let scheme = FecScheme::for_overhead(4, 50);

        let sizes: [(usize, usize); 6] =
            [(8, 1), (8, 66), (8, 500), (0, 200), (100, 300), (200, 5)];

        for (prefix_len, body_len) in sizes {
            let prefix: Vec<u8> = (0..prefix_len)
                .map(|byte| u8::try_from(byte % 251).unwrap() ^ 0x5A)
                .collect();
            let body: Vec<u8> = (0..body_len)
                .map(|byte| u8::try_from(byte * 7 % 251).unwrap())
                .collect();
            let joined = [prefix.clone(), body.clone()].concat();
            let mut split = video(scheme);
            let mut whole = video(scheme);

            let from_parts = split.packetize_prefixed(3, true, &prefix, &body).unwrap();
            let from_joined = whole.packetize(3, true, &joined).unwrap();

            assert_eq!(
                from_parts, from_joined,
                "prefix {prefix_len} body {body_len}"
            );
        }
    }

    #[test]
    fn an_empty_prefix_and_body_is_an_empty_frame() {
        assert!(matches!(
            video(FecScheme::None).packetize_prefixed(0, false, &[], &[]),
            Err(NetError::EmptyFrame)
        ));
    }
}
