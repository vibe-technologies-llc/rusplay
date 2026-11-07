use std::{
    fmt,
    io::{self, IoSlice},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6},
    ops::Range,
    os::fd::AsRawFd,
    ptr,
    time::Duration,
};

use rustix::{
    io::Errno,
    net::{MMsgHdr, SendAncillaryBuffer, SendFlags, addr::SocketAddrArg, sendmmsg},
};

use crate::{DatagramArena, NetError, Transport, transport::seal_with};

const MAX_MESSAGES_PER_CALL: usize = 1024;
const MIN_READ_TIMEOUT: Duration = Duration::from_micros(1);
const SOCKADDR_LEN: usize = 28;
const SOCKADDR_LEN_C: libc::socklen_t = 28;

#[derive(Debug)]
pub struct RecvBatch {
    buffers: Vec<Vec<u8>>,
    lengths: Vec<usize>,
    sources: Vec<SocketAddr>,
    filled: usize,
    call: ReceiveCall,
}

struct ReceiveCall {
    names: Vec<[u8; SOCKADDR_LEN]>,
    iovecs: Vec<libc::iovec>,
    headers: Vec<libc::mmsghdr>,
}

impl fmt::Debug for ReceiveCall {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReceiveCall")
            .field("slots", &self.names.len())
            .finish_non_exhaustive()
    }
}

impl ReceiveCall {
    fn new(datagrams: usize) -> Self {
        let empty_iovec = libc::iovec {
            iov_base: ptr::null_mut(),
            iov_len: 0,
        };
        let empty_header = libc::mmsghdr {
            msg_hdr: libc::msghdr {
                msg_name: ptr::null_mut(),
                msg_namelen: 0,
                msg_iov: ptr::null_mut(),
                msg_iovlen: 0,
                msg_control: ptr::null_mut(),
                msg_controllen: 0,
                msg_flags: 0,
            },
            msg_len: 0,
        };

        Self {
            names: vec![[0; SOCKADDR_LEN]; datagrams],
            iovecs: vec![empty_iovec; datagrams],
            headers: vec![empty_header; datagrams],
        }
    }

    fn arm(&mut self, buffers: &mut [Vec<u8>]) {
        let slots = self
            .names
            .iter_mut()
            .zip(self.iovecs.iter_mut())
            .zip(self.headers.iter_mut())
            .zip(buffers);

        for (((name, iovec), header), buffer) in slots {
            iovec.iov_base = buffer.as_mut_ptr().cast();
            iovec.iov_len = buffer.len();
            header.msg_hdr.msg_name = ptr::from_mut(name).cast();
            header.msg_hdr.msg_namelen = SOCKADDR_LEN_C;
            header.msg_hdr.msg_iov = ptr::from_mut(iovec);
            header.msg_hdr.msg_iovlen = 1;
            header.msg_hdr.msg_flags = 0;
            header.msg_len = 0;
        }
    }
}

impl RecvBatch {
    pub fn new(datagrams: usize, datagram_capacity: usize) -> Self {
        Self {
            buffers: vec![vec![0; datagram_capacity]; datagrams],
            lengths: vec![0; datagrams],
            sources: vec![SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0); datagrams],
            filled: 0,
            call: ReceiveCall::new(datagrams),
        }
    }

    pub fn capacity(&self) -> usize {
        self.buffers.len()
    }

    pub fn len(&self) -> usize {
        self.filled
    }

    pub fn is_empty(&self) -> bool {
        self.filled == 0
    }

    pub fn iter(&self) -> impl Iterator<Item = (&[u8], SocketAddr)> {
        (0..self.filled).filter_map(|index| {
            let length = self.lengths[index];
            (length > 0).then(|| (&self.buffers[index][..length], self.sources[index]))
        })
    }
}

impl Transport {
    pub fn send_batch(&self, datagrams: &[Vec<u8>], peer: SocketAddr) -> Result<usize, NetError> {
        let budget = self.max_datagram_len(&peer);
        if let Some(oversized) = datagrams.iter().find(|datagram| datagram.len() > budget) {
            return Err(NetError::DatagramTooLarge {
                size: oversized.len(),
                mtu: self.mtu(),
            });
        }

        let cipher = self.current_cipher();
        let sealed = datagrams
            .iter()
            .map(|datagram| seal_with(cipher.as_deref(), datagram))
            .collect::<Result<Vec<_>, _>>()?;
        let wire: Vec<[IoSlice<'_>; 2]> = sealed
            .iter()
            .map(|datagram| [IoSlice::new(datagram.as_ref()), IoSlice::new(&[])])
            .collect();

        self.send_wire(&wire, peer)
    }

    pub fn send_arena(
        &self,
        arena: &mut DatagramArena,
        range: Range<usize>,
        peer: SocketAddr,
    ) -> Result<usize, NetError> {
        let budget = self.max_datagram_len(&peer);
        if let Some(oversized) = range
            .clone()
            .map(|index| arena.plain_len(index))
            .find(|len| *len > budget)
        {
            return Err(NetError::DatagramTooLarge {
                size: oversized,
                mtu: self.mtu(),
            });
        }

        if let Some(cipher) = self.current_cipher() {
            arena.seal(range.clone(), &cipher)?;
        }
        let wire: Vec<[IoSlice<'_>; 2]> = range.map(|index| arena.wire(index)).collect();

        self.send_wire(&wire, peer)
    }

    fn send_wire(&self, wire: &[[IoSlice<'_>; 2]], peer: SocketAddr) -> Result<usize, NetError> {
        let address = peer.as_any();
        let mut sent = 0;

        while sent < wire.len() {
            let chunk = &wire[sent..wire.len().min(sent + MAX_MESSAGES_PER_CALL)];
            let mut controls: Vec<SendAncillaryBuffer<'_, '_, '_>> = chunk
                .iter()
                .map(|_| SendAncillaryBuffer::default())
                .collect();
            let mut messages: Vec<MMsgHdr<'_>> = chunk
                .iter()
                .zip(controls.iter_mut())
                .map(|(slices, control)| MMsgHdr::new_with_addr(&address, slices, control))
                .collect();

            match sendmmsg(self.socket(), &mut messages, SendFlags::DONTWAIT) {
                Ok(0) | Err(Errno::AGAIN) => break,
                Ok(count) => sent += count,
                Err(_) if sent > 0 => break,
                Err(errno) => {
                    return Err(NetError::Send {
                        peer,
                        source: errno.into(),
                    });
                }
            }
        }

        Ok(sent)
    }

    pub fn recv_batch_timeout(
        &self,
        batch: &mut RecvBatch,
        timeout: Duration,
    ) -> Result<usize, NetError> {
        self.set_read_timeout_cached(timeout.max(MIN_READ_TIMEOUT))?;

        self.recv_batch(batch)
    }

    pub fn recv_batch(&self, batch: &mut RecvBatch) -> Result<usize, NetError> {
        batch.filled = 0;
        if batch.capacity() == 0 {
            return Ok(0);
        }

        batch.call.arm(&mut batch.buffers);
        let headers = &mut batch.call.headers;
        let count = libc::c_uint::try_from(headers.len()).unwrap_or(libc::c_uint::MAX);

        let received = unsafe {
            libc::recvmmsg(
                self.socket().as_raw_fd(),
                headers.as_mut_ptr(),
                count,
                libc::MSG_WAITFORONE,
                ptr::null_mut(),
            )
        };

        if received < 0 {
            let error = io::Error::last_os_error();
            return if error.kind() == io::ErrorKind::WouldBlock {
                Ok(0)
            } else {
                Err(NetError::Receive(error))
            };
        }

        let received = usize::try_from(received).unwrap_or(0);
        for index in 0..received {
            let header = &batch.call.headers[index];
            let truncated = header.msg_hdr.msg_flags & libc::MSG_TRUNC != 0;
            let length_received = header.msg_len as usize;
            let source = parse_sockaddr(&batch.call.names[index][..]);

            let length = match (truncated, source) {
                (false, Some(source)) => {
                    batch.sources[index] = source;
                    self.unseal(&mut batch.buffers[index], length_received)
                }
                _ => None,
            };
            batch.lengths[index] = length.unwrap_or(0);
        }
        batch.filled = received;

        Ok(batch.iter().count())
    }
}

fn parse_sockaddr(bytes: &[u8]) -> Option<SocketAddr> {
    let family = i32::from(u16::from_ne_bytes([bytes[0], bytes[1]]));
    let port = u16::from_be_bytes([bytes[2], bytes[3]]);

    if family == libc::AF_INET {
        let octets: [u8; 4] = bytes[4..8].try_into().ok()?;
        return Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::from(octets)), port));
    }

    if family == libc::AF_INET6 {
        let flow_info = u32::from_be_bytes(bytes[4..8].try_into().ok()?);
        let octets: [u8; 16] = bytes[8..24].try_into().ok()?;
        let scope_id = u32::from_ne_bytes(bytes[24..28].try_into().ok()?);
        return Some(SocketAddr::V6(SocketAddrV6::new(
            Ipv6Addr::from(octets),
            port,
            flow_info,
            scope_id,
        )));
    }

    None
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::TransportOptions;

    const LOOPBACK_V4: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
    const LOOPBACK_V6: SocketAddr = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 0);

    fn bind(addr: SocketAddr) -> Transport {
        Transport::bind(addr, TransportOptions::default()).unwrap()
    }

    fn datagrams(count: usize) -> Vec<Vec<u8>> {
        (0..count)
            .map(|index| vec![u8::try_from(index).unwrap(); 100 + index])
            .collect()
    }

    fn collect(batch: &RecvBatch) -> Vec<Vec<u8>> {
        batch
            .iter()
            .map(|(datagram, _)| datagram.to_vec())
            .collect()
    }

    #[test]
    fn a_reused_batch_reports_the_source_of_every_call() {
        let first = bind(LOOPBACK_V4);
        let second = bind(LOOPBACK_V4);
        let receiver = bind(LOOPBACK_V4);
        let peer = receiver.local_addr().unwrap();
        let mut batch = RecvBatch::new(4, 2048);
        let timeout = Duration::from_millis(500);

        first.send_batch(&datagrams(2), peer).unwrap();
        receiver.recv_batch_timeout(&mut batch, timeout).unwrap();
        let from_first: Vec<SocketAddr> = batch.iter().map(|(_, from)| from).collect();

        second.send_batch(&datagrams(1), peer).unwrap();
        receiver.recv_batch_timeout(&mut batch, timeout).unwrap();
        let from_second: Vec<SocketAddr> = batch.iter().map(|(_, from)| from).collect();

        let first_addr = first.local_addr().unwrap();
        let second_addr = second.local_addr().unwrap();
        assert_eq!(from_first, [first_addr; 2]);
        assert_eq!(from_second, [second_addr]);
    }

    #[test]
    fn an_unchanged_timeout_is_not_applied_again() {
        let receiver = bind(LOOPBACK_V4);
        let timeout = Duration::from_millis(30);

        receiver.set_read_timeout_cached(timeout).unwrap();
        receiver
            .socket()
            .set_read_timeout(Some(Duration::from_secs(60)))
            .unwrap();
        receiver.set_read_timeout_cached(timeout).unwrap();
        let skipped = receiver.socket().read_timeout().unwrap();
        receiver
            .set_read_timeout_cached(Duration::from_millis(40))
            .unwrap();
        let changed = receiver.socket().read_timeout().unwrap();

        assert_eq!(skipped, Some(Duration::from_secs(60)));
        assert_eq!(changed, Some(Duration::from_millis(40)));
    }

    #[test]
    fn batch_arrives_intact_and_in_order() {
        let sender = bind(LOOPBACK_V4);
        let receiver = bind(LOOPBACK_V4);
        let peer = receiver.local_addr().unwrap();
        let outgoing = datagrams(10);

        let sent = sender.send_batch(&outgoing, peer).unwrap();

        let mut batch = RecvBatch::new(16, 2048);
        let mut collected = Vec::new();
        while collected.len() < outgoing.len() {
            receiver.recv_batch(&mut batch).unwrap();
            collected.extend(collect(&batch));
        }
        assert_eq!(sent, 10);
        assert_eq!(collected, outgoing);
    }

    #[test]
    fn receive_reports_the_sender_address() {
        let sender = bind(LOOPBACK_V4);
        let receiver = bind(LOOPBACK_V4);
        let peer = receiver.local_addr().unwrap();

        sender.send_batch(&datagrams(3), peer).unwrap();

        let mut batch = RecvBatch::new(8, 2048);
        receiver.recv_batch(&mut batch).unwrap();
        let sources: Vec<SocketAddr> = batch.iter().map(|(_, source)| source).collect();
        assert_ne!(sources.len(), 0);
        assert!(
            sources
                .iter()
                .all(|source| *source == sender.local_addr().unwrap())
        );
    }

    #[test]
    fn ipv6_sources_are_parsed() {
        let sender = bind(LOOPBACK_V6);
        let receiver = bind(LOOPBACK_V6);
        let peer = receiver.local_addr().unwrap();

        sender.send_batch(&datagrams(2), peer).unwrap();

        let mut batch = RecvBatch::new(8, 2048);
        receiver.recv_batch(&mut batch).unwrap();
        assert!(!batch.is_empty());
        assert_eq!(batch.iter().next().unwrap().1, sender.local_addr().unwrap());
    }

    #[test]
    fn batches_larger_than_the_receive_batch_span_several_calls() {
        let sender = bind(LOOPBACK_V4);
        let receiver = bind(LOOPBACK_V4);
        let peer = receiver.local_addr().unwrap();
        let outgoing = datagrams(40);

        sender.send_batch(&outgoing, peer).unwrap();

        let mut batch = RecvBatch::new(4, 2048);
        let mut collected = Vec::new();
        while collected.len() < outgoing.len() {
            assert!(receiver.recv_batch(&mut batch).unwrap() <= 4);
            collected.extend(collect(&batch));
        }
        assert_eq!(collected, outgoing);
    }

    #[test]
    fn nonblocking_receive_without_traffic_returns_zero() {
        let receiver = bind(LOOPBACK_V4);
        receiver.set_nonblocking(true).unwrap();
        let mut batch = RecvBatch::new(4, 2048);

        assert_eq!(receiver.recv_batch(&mut batch).unwrap(), 0);
        assert!(batch.is_empty());
    }

    #[test]
    fn oversized_datagram_rejects_the_whole_batch() {
        let sender = bind(LOOPBACK_V4);
        let peer = sender.local_addr().unwrap();
        let mut outgoing = datagrams(3);
        outgoing.push(vec![0; 5000]);

        assert!(matches!(
            sender.send_batch(&outgoing, peer),
            Err(NetError::DatagramTooLarge { size: 5000, .. })
        ));
        sender.set_nonblocking(true).unwrap();
        assert!(sender.try_recv_from(&mut [0; 16]).unwrap().is_none());
    }

    #[test]
    fn truncated_datagrams_are_dropped_from_the_batch() {
        let sender = bind(LOOPBACK_V4);
        let receiver = bind(LOOPBACK_V4);
        let peer = receiver.local_addr().unwrap();
        sender
            .send_batch(&[vec![1; 300], vec![2; 50]], peer)
            .unwrap();

        let mut batch = RecvBatch::new(4, 100);
        let mut collected = Vec::new();
        while collected.is_empty() {
            receiver.recv_batch(&mut batch).unwrap();
            collected.extend(collect(&batch));
            std::thread::sleep(Duration::from_millis(1));
        }

        assert!(collected.iter().all(|datagram| datagram == &vec![2; 50]));
    }

    #[test]
    fn empty_batch_sends_nothing() {
        let sender = bind(LOOPBACK_V4);
        let peer = sender.local_addr().unwrap();

        assert_eq!(sender.send_batch(&[], peer).unwrap(), 0);
    }

    #[test]
    fn send_batch_carries_a_fragmented_frame_end_to_end() {
        use std::time::Instant;

        use rusplay_protocol::StreamKind;

        use crate::{FecScheme, Packetizer, Reassembler, ReassemblyEvent, ReassemblyLimits};

        let sender = bind(LOOPBACK_V4);
        let receiver = bind(LOOPBACK_V4);
        let peer = receiver.local_addr().unwrap();
        let mut packetizer = Packetizer::new(
            StreamKind::Video,
            sender.max_datagram_len(&peer),
            FecScheme::for_overhead(16, 20),
        )
        .unwrap();
        let mut reassembler = Reassembler::new(StreamKind::Video, ReassemblyLimits::default());
        let data = vec![9u8; 40_000];

        let datagrams = packetizer.packetize(1, true, &data).unwrap();
        let expected = datagrams.len();
        sender.send_batch(&datagrams, peer).unwrap();

        let mut batch = RecvBatch::new(64, 2048);
        let mut events = Vec::new();
        let mut seen = 0;
        while seen < expected {
            receiver.recv_batch(&mut batch).unwrap();
            for (datagram, _) in batch.iter() {
                reassembler
                    .push(datagram, Instant::now(), &mut events)
                    .unwrap();
                seen += 1;
            }
        }

        assert!(matches!(
            events.as_slice(),
            [ReassemblyEvent::Frame(frame)] if frame.data == data
        ));
    }

    #[test]
    fn sealed_batches_are_opened_by_the_receiver_and_forgeries_are_dropped() {
        use rusplay_protocol::StreamKind;

        use crate::{SessionKeys, requires_sealing};

        let sender = bind(LOOPBACK_V4);
        let receiver = bind(LOOPBACK_V4);
        let peer = receiver.local_addr().unwrap();
        sender.install_session(&SessionKeys::new([1; 32], [2; 32]));
        receiver.install_session(&SessionKeys::new([2; 32], [1; 32]));
        assert!(requires_sealing(StreamKind::Video));
        let outgoing: Vec<Vec<u8>> = (0..6u8)
            .map(|index| {
                let mut tx = crate::Packetizer::new(
                    StreamKind::Video,
                    sender.max_datagram_len(&peer),
                    crate::FecScheme::None,
                )
                .unwrap();
                tx.packetize(u32::from(index), false, &[index; 200])
                    .unwrap()
                    .remove(0)
            })
            .collect();

        sender.send_batch(&outgoing, peer).unwrap();

        let mut batch = RecvBatch::new(16, 2048);
        let mut collected = Vec::new();
        while collected.len() < outgoing.len() {
            receiver.recv_batch(&mut batch).unwrap();
            collected.extend(collect(&batch));
        }
        assert_eq!(collected, outgoing);
        assert_eq!(receiver.rejected_datagrams(), 0);

        sender.socket().send_to(&outgoing[0], peer).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        receiver.recv_batch(&mut batch).unwrap();
        assert_eq!(batch.iter().count(), 0);
        assert_eq!(receiver.rejected_datagrams(), 1);
    }

    #[test]
    fn batch_receive_gives_up_after_the_timeout() {
        let host = bind(LOOPBACK_V4);
        let mut batch = RecvBatch::new(4, 2048);
        let started = Instant::now();

        let count = host
            .recv_batch_timeout(&mut batch, Duration::from_millis(20))
            .unwrap();

        assert_eq!(count, 0);
        assert!(batch.is_empty());
        assert!(started.elapsed() >= Duration::from_millis(15));
    }

    #[test]
    fn batch_receive_with_timeout_returns_waiting_datagrams() {
        let host = bind(LOOPBACK_V4);
        let sender = bind(LOOPBACK_V4);
        let mut batch = RecvBatch::new(4, 2048);
        sender
            .send_batch(&datagrams(2), host.local_addr().unwrap())
            .unwrap();

        let count = host.recv_batch_timeout(&mut batch, Duration::ZERO).unwrap();

        assert!(count >= 1);
        assert_eq!(collect(&batch)[0], datagrams(1)[0]);
    }

    fn arena_of(sender: &Transport, peer: SocketAddr, frame: u32, data: &[u8]) -> DatagramArena {
        use rusplay_protocol::StreamKind;

        use crate::{FecScheme, Packetizer};

        let mut arena = DatagramArena::new();
        Packetizer::new(
            StreamKind::Video,
            sender.max_datagram_len(&peer),
            FecScheme::for_overhead(8, 25),
        )
        .unwrap()
        .packetize_into(&mut arena, frame, true, &[], data)
        .unwrap();

        arena
    }

    fn receive_all(receiver: &Transport, expected: usize) -> Vec<Vec<u8>> {
        let mut batch = RecvBatch::new(64, 2048);
        let mut collected = Vec::new();
        while collected.len() < expected {
            receiver.recv_batch(&mut batch).unwrap();
            collected.extend(collect(&batch));
        }

        collected
    }

    #[test]
    fn an_arena_travels_in_the_clear_without_a_session() {
        let sender = bind(LOOPBACK_V4);
        let receiver = bind(LOOPBACK_V4);
        let peer = receiver.local_addr().unwrap();
        let data: Vec<u8> = (0..=u8::MAX).cycle().take(9_000).collect();
        let mut arena = arena_of(&sender, peer, 1, &data);
        let expected = arena.to_vecs();

        let count = expected.len();

        let sent = sender.send_arena(&mut arena, 0..count, peer).unwrap();

        assert_eq!(sent, expected.len());
        assert_eq!(receive_all(&receiver, expected.len()), expected);
    }

    #[test]
    fn a_sealed_arena_is_opened_by_the_receiver() {
        use crate::SessionKeys;

        let sender = bind(LOOPBACK_V4);
        let receiver = bind(LOOPBACK_V4);
        let peer = receiver.local_addr().unwrap();
        sender.install_session(&SessionKeys::new([1; 32], [2; 32]));
        receiver.install_session(&SessionKeys::new([2; 32], [1; 32]));
        let data: Vec<u8> = (0..=u8::MAX).rev().cycle().take(9_000).collect();
        let mut arena = arena_of(&sender, peer, 1, &data);
        let expected = arena.to_vecs();

        let count = expected.len();

        let sent = sender.send_arena(&mut arena, 0..count, peer).unwrap();

        assert_eq!(sent, expected.len());
        assert_eq!(receive_all(&receiver, expected.len()), expected);
        assert_eq!(receiver.rejected_datagrams(), 0);
    }

    #[test]
    fn a_partly_sent_arena_range_continues_where_it_stopped() {
        use crate::SessionKeys;

        let sender = bind(LOOPBACK_V4);
        let receiver = bind(LOOPBACK_V4);
        let peer = receiver.local_addr().unwrap();
        sender.install_session(&SessionKeys::new([1; 32], [2; 32]));
        receiver.install_session(&SessionKeys::new([2; 32], [1; 32]));
        let mut arena = arena_of(&sender, peer, 1, &[5; 6_000]);
        let expected = arena.to_vecs();
        let split = expected.len() / 2;

        sender.send_arena(&mut arena, 0..split, peer).unwrap();
        sender
            .send_arena(&mut arena, split..expected.len(), peer)
            .unwrap();

        assert_eq!(receive_all(&receiver, expected.len()), expected);
    }

    #[test]
    fn an_oversized_arena_datagram_is_refused() {
        use rusplay_protocol::{PacketFlags, PacketHeader, ProtocolVersion, StreamKind};

        let sender = bind(LOOPBACK_V4);
        let peer = bind(LOOPBACK_V4).local_addr().unwrap();
        let header = PacketHeader {
            version: ProtocolVersion::CURRENT,
            stream: StreamKind::Video,
            flags: PacketFlags::empty(),
            sequence: 0,
            frame: 0,
            fragment_index: 0,
            fragment_count: 1,
        };
        let mut arena = DatagramArena::new();
        arena
            .push(&header, None, sender.max_datagram_len(&peer))
            .unwrap();

        assert!(matches!(
            sender.send_arena(&mut arena, 0..1, peer),
            Err(NetError::DatagramTooLarge { .. })
        ));
    }
}
