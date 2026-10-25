use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::{Duration, Instant},
};

use rusplay_net::{
    FecScheme, Packetizer, Reassembler, ReassemblyEvent, ReassemblyLimits, Transport,
};
use rusplay_protocol::StreamKind;

const LOOPBACK: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
const MTU: u16 = 1500;

fn frame_data(len: usize) -> Vec<u8> {
    (0..len)
        .map(|index| u8::try_from(index * 31 % 256).unwrap())
        .collect()
}

fn pair() -> (Transport, Transport) {
    let sender = Transport::bind(LOOPBACK, MTU).unwrap();
    let receiver = Transport::bind(LOOPBACK, MTU).unwrap();
    (sender, receiver)
}

fn receive_all(
    receiver: &Transport,
    reassembler: &mut Reassembler,
    expected: usize,
) -> Vec<ReassemblyEvent> {
    let mut events = Vec::new();
    let mut buf = vec![0; 2048];

    for _ in 0..expected {
        let (len, _) = receiver.recv_from(&mut buf).unwrap();
        reassembler
            .push(&buf[..len], Instant::now(), &mut events)
            .unwrap();
    }

    events
}

#[test]
fn keyframe_survives_a_lossy_loopback_hop() {
    let (sender, receiver) = pair();
    let peer = receiver.local_addr().unwrap();
    let mut packetizer = Packetizer::new(
        StreamKind::Video,
        sender.max_datagram_len(&peer),
        FecScheme::for_overhead(32, 20),
    )
    .unwrap();
    let mut reassembler = Reassembler::new(StreamKind::Video, ReassemblyLimits::default());
    let data = frame_data(60_000);

    let datagrams = packetizer.packetize(1, true, &data).unwrap();
    let mut sent = 0;
    for (index, datagram) in datagrams.iter().enumerate() {
        if index % 40 == 7 {
            continue;
        }
        sender.send_to(datagram, peer).unwrap();
        sent += 1;
    }

    let events = receive_all(&receiver, &mut reassembler, sent);

    let frame = events
        .iter()
        .find_map(|event| match event {
            ReassemblyEvent::Frame(frame) => Some(frame),
            _ => None,
        })
        .unwrap();
    assert_eq!(frame.data, data);
    assert!(frame.keyframe);
    assert!(frame.recovered_fragments > 0);
}

#[test]
fn unrecoverable_frame_is_reported_lost() {
    let (sender, receiver) = pair();
    let peer = receiver.local_addr().unwrap();
    let mut packetizer = Packetizer::new(
        StreamKind::Video,
        sender.max_datagram_len(&peer),
        FecScheme::None,
    )
    .unwrap();
    let limits = ReassemblyLimits {
        frame_timeout: Duration::from_millis(1),
        ..ReassemblyLimits::default()
    };
    let mut reassembler = Reassembler::new(StreamKind::Video, limits);

    let datagrams = packetizer.packetize(1, false, &frame_data(10_000)).unwrap();
    for datagram in datagrams.iter().skip(1) {
        sender.send_to(datagram, peer).unwrap();
    }

    let mut events = receive_all(&receiver, &mut reassembler, datagrams.len() - 1);
    reassembler.poll_timeouts(Instant::now() + Duration::from_secs(1), &mut events);

    assert!(events.contains(&ReassemblyEvent::FrameLost { frame: 1 }));
}
