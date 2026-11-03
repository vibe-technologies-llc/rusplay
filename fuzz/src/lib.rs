use rusplay_core::{Codec, EncodedFrame, FrameBytes, Timestamp};
use rusplay_decode::VideoDecoder;
use rusplay_protocol::{PacketHeader, ProtocolVersion, StreamKind};

const VERSION_END: usize = 2;
const STREAM_OFFSET: usize = 2;
const FRAGMENT_INDEX: usize = 12;
const FRAGMENT_COUNT: usize = 14;

pub fn stamped(data: &[u8], stream: StreamKind) -> Vec<u8> {
    let mut datagram = data.to_vec();
    if datagram.len() < PacketHeader::SIZE {
        return datagram;
    }

    datagram[..VERSION_END].copy_from_slice(&ProtocolVersion::CURRENT.0.to_be_bytes());
    datagram[STREAM_OFFSET] = stream as u8;

    let count = u16::from_be_bytes([datagram[FRAGMENT_COUNT], datagram[FRAGMENT_COUNT + 1]]).max(1);
    let index =
        u16::from_be_bytes([datagram[FRAGMENT_INDEX], datagram[FRAGMENT_INDEX + 1]]) % count;
    datagram[FRAGMENT_INDEX..FRAGMENT_INDEX + 2].copy_from_slice(&index.to_be_bytes());
    datagram[FRAGMENT_COUNT..FRAGMENT_COUNT + 2].copy_from_slice(&count.to_be_bytes());

    datagram
}

pub fn datagram_list(data: &[u8]) -> Vec<&[u8]> {
    let mut rest = data;
    let mut datagrams = Vec::new();

    while let Some((&len, tail)) = rest.split_first() {
        let (datagram, remainder) = tail.split_at(usize::from(len).min(tail.len()));
        datagrams.push(datagram);
        rest = remainder;
    }

    datagrams
}

const LENGTH_PREFIX: usize = 2;
const PACKET_INTERVAL_MICROS: u64 = 16_667;

pub fn packets(data: &[u8]) -> Vec<&[u8]> {
    let mut rest = data;
    let mut packets = Vec::new();

    while let Some((prefix, tail)) = rest.split_at_checked(LENGTH_PREFIX) {
        let length = usize::from(u16::from_be_bytes([prefix[0], prefix[1]]));
        let (packet, remainder) = tail.split_at(length.min(tail.len()));
        packets.push(packet);
        rest = remainder;
    }

    packets
}

pub fn feed(decoder: &mut dyn VideoDecoder, codec: Codec, data: &[u8]) {
    for (index, packet) in packets(data).into_iter().enumerate() {
        let micros = u64::try_from(index).unwrap_or(0) * PACKET_INTERVAL_MICROS;
        let frame = EncodedFrame {
            codec,
            timestamp: Timestamp::from_micros(micros),
            keyframe: index == 0,
            data: FrameBytes::skipping(packet.to_vec(), 0),
        };

        let _ = decoder.decode(&frame);
    }

    let _ = decoder.flush();
}
