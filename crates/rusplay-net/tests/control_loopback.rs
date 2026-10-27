use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    thread,
    time::{Duration, Instant},
};

use rusplay_net::{
    ControlChannel, ControlConfig, ControlEvent, FecScheme, Packetizer, Reassembler,
    ReassemblyEvent, ReassemblyLimits, Transport, TransportOptions,
};
use rusplay_protocol::{ControlMessage, PacketHeader, StreamKind};

const LOOPBACK: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);

struct Endpoint {
    transport: Transport,
    channel: ControlChannel,
    events: Vec<ControlEvent>,
    media: Vec<Vec<u8>>,
}

impl Endpoint {
    fn new() -> Self {
        let transport = Transport::bind(LOOPBACK, TransportOptions::default()).unwrap();
        transport.set_nonblocking(true).unwrap();

        Self {
            transport,
            channel: ControlChannel::new(ControlConfig {
                retransmit_timeout: Duration::from_millis(10),
                max_retransmits: 20,
            }),
            events: Vec::new(),
            media: Vec::new(),
        }
    }

    fn addr(&self) -> SocketAddr {
        self.transport.local_addr().unwrap()
    }

    fn pump(&mut self, peer: SocketAddr, now: Instant) {
        let mut buf = vec![0; 2048];

        while let Some((len, _)) = self.transport.try_recv_from(&mut buf).unwrap() {
            let datagram = &buf[..len];
            match PacketHeader::read(datagram).unwrap().stream {
                StreamKind::Control => self
                    .channel
                    .handle_datagram(datagram, now, &mut self.events)
                    .unwrap(),
                _ => self.media.push(datagram.to_vec()),
            }
        }

        self.channel.poll(now, &mut self.events);
        self.channel.flush(&self.transport, peer).unwrap();
    }
}

#[test]
fn control_message_survives_a_dropped_first_datagram() {
    let mut a = Endpoint::new();
    let mut b = Endpoint::new();
    let (addr_a, addr_b) = (a.addr(), b.addr());

    let id = a
        .channel
        .send(
            ControlMessage::RequestKeyframe { frame: 42 },
            Instant::now(),
        )
        .unwrap();
    let mut dropped = Vec::new();
    a.channel.take_transmits(&mut dropped);
    assert_eq!(dropped.len(), 1);

    let deadline = Instant::now() + Duration::from_secs(5);
    while a.channel.in_flight() > 0 {
        assert!(
            Instant::now() < deadline,
            "control message never acknowledged"
        );
        a.pump(addr_b, Instant::now());
        b.pump(addr_a, Instant::now());
        thread::sleep(Duration::from_millis(2));
    }

    assert_eq!(
        b.events,
        [ControlEvent::Received(ControlMessage::RequestKeyframe {
            frame: 42
        })]
    );
    assert_eq!(a.events, [ControlEvent::Acked { id }]);
}

#[test]
fn control_traffic_shares_the_socket_with_media() {
    let mut a = Endpoint::new();
    let mut b = Endpoint::new();
    let (addr_a, addr_b) = (a.addr(), b.addr());
    let mut packetizer = Packetizer::new(
        StreamKind::Video,
        a.transport.max_datagram_len(&addr_b),
        FecScheme::None,
    )
    .unwrap();
    let mut reassembler = Reassembler::new(StreamKind::Video, ReassemblyLimits::default());
    let frame = vec![5u8; 20_000];

    a.channel
        .send(ControlMessage::BitrateHint { kbps: 50_000 }, Instant::now())
        .unwrap();
    a.channel.flush(&a.transport, addr_b).unwrap();
    for datagram in packetizer.packetize(0, false, &frame).unwrap() {
        a.transport.send_to(&datagram, addr_b).unwrap();
    }

    let deadline = Instant::now() + Duration::from_secs(5);
    while a.channel.in_flight() > 0 {
        assert!(
            Instant::now() < deadline,
            "control message never acknowledged"
        );
        a.pump(addr_b, Instant::now());
        b.pump(addr_a, Instant::now());
        thread::sleep(Duration::from_millis(2));
    }

    let mut events = Vec::new();
    for datagram in &b.media {
        reassembler
            .push(datagram, Instant::now(), &mut events)
            .unwrap();
    }

    assert!(matches!(
        events.as_slice(),
        [ReassemblyEvent::Frame(received)] if received.data == frame
    ));
    assert_eq!(
        b.events,
        [ControlEvent::Received(ControlMessage::BitrateHint {
            kbps: 50_000
        })]
    );
}
