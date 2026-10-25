use rusplay_protocol::{FecHeader, PacketFlags, PacketHeader, ProtocolVersion, StreamKind};

use crate::{FecScheme, NetError};

#[derive(Debug, Clone)]
pub struct Packetizer {
    stream: StreamKind,
    shard_len: usize,
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
            scheme,
            next_sequence: 0,
        })
    }

    pub const fn shard_len(&self) -> usize {
        self.shard_len
    }

    pub const fn next_sequence(&self) -> u32 {
        self.next_sequence
    }

    pub fn max_frame_bytes(&self) -> usize {
        self.shard_len * usize::from(u16::MAX)
    }

    pub fn packetize(
        &mut self,
        frame: u32,
        keyframe: bool,
        data: &[u8],
    ) -> Result<Vec<Vec<u8>>, NetError> {
        if data.is_empty() {
            return Err(NetError::EmptyFrame);
        }

        let fragment_count = u16::try_from(data.len().div_ceil(self.shard_len)).map_err(|_| {
            NetError::FrameTooLarge {
                bytes: data.len(),
                max_bytes: self.max_frame_bytes(),
            }
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

        for (index, chunk) in data.chunks(self.shard_len).enumerate() {
            let flags = if index + 1 == usize::from(fragment_count) {
                base_flags.with(PacketFlags::LAST_FRAGMENT)
            } else {
                base_flags
            };
            let header = self.next_header(self.stream, flags, frame, index, fragment_count);

            datagrams.push(build_datagram(&header, None, chunk)?);
        }

        let fec_packet_count =
            u16::try_from(parity_count).map_err(|_| NetError::FrameTooLarge {
                bytes: data.len(),
                max_bytes: self.max_frame_bytes(),
            })?;
        let mut fec_index = 0usize;

        for block in &blocks {
            let first = usize::from(block.first_fragment);
            let shards: Vec<Vec<u8>> = (first..first + usize::from(block.data_shards))
                .map(|index| self.padded_shard(data, index))
                .collect();
            let shard_refs: Vec<&[u8]> = shards.iter().map(Vec::as_slice).collect();
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

    fn padded_shard(&self, data: &[u8], index: usize) -> Vec<u8> {
        let start = index * self.shard_len;
        let end = (start + self.shard_len).min(data.len());
        let mut shard = data[start..end].to_vec();
        shard.resize(self.shard_len, 0);
        shard
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
    let mut datagram = vec![0; PacketHeader::SIZE + fec_len + payload.len()];

    header.write(&mut datagram)?;
    if let Some(fec_header) = fec_header {
        fec_header.write(&mut datagram[PacketHeader::SIZE..])?;
    }
    datagram[PacketHeader::SIZE + fec_len..].copy_from_slice(payload);

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
}
