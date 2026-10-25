use std::{
    io::{self, IoSlice},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6},
    os::fd::AsRawFd,
    ptr,
    time::Duration,
};

use rustix::{
    io::Errno,
    net::{MMsgHdr, SendAncillaryBuffer, SendFlags, addr::SocketAddrArg, sendmmsg},
};

use crate::{NetError, Transport};

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
}

impl RecvBatch {
    pub fn new(datagrams: usize, datagram_capacity: usize) -> Self {
        Self {
            buffers: vec![vec![0; datagram_capacity]; datagrams],
            lengths: vec![0; datagrams],
            sources: vec![SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0); datagrams],
            filled: 0,
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

        let sealed = datagrams
            .iter()
            .map(|datagram| self.seal(datagram))
            .collect::<Result<Vec<_>, _>>()?;
        let address = peer.as_any();
        let mut sent = 0;

        while sent < sealed.len() {
            let chunk = &sealed[sent..sealed.len().min(sent + MAX_MESSAGES_PER_CALL)];
            let slices: Vec<[IoSlice<'_>; 1]> = chunk
                .iter()
                .map(|datagram| [IoSlice::new(datagram.as_ref())])
                .collect();
            let mut controls: Vec<SendAncillaryBuffer<'_, '_, '_>> = chunk
                .iter()
                .map(|_| SendAncillaryBuffer::default())
                .collect();
            let mut messages: Vec<MMsgHdr<'_>> = slices
                .iter()
                .zip(controls.iter_mut())
                .map(|(slice, control)| MMsgHdr::new_with_addr(&address, slice, control))
                .collect();

            match sendmmsg(self.socket(), &mut messages, SendFlags::empty()) {
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
        self.socket()
            .set_read_timeout(Some(timeout.max(MIN_READ_TIMEOUT)))
            .map_err(NetError::ReadTimeout)?;

        self.recv_batch(batch)
    }

    pub fn recv_batch(&self, batch: &mut RecvBatch) -> Result<usize, NetError> {
        batch.filled = 0;
        if batch.capacity() == 0 {
            return Ok(0);
        }

        let mut names = vec![[0u8; SOCKADDR_LEN]; batch.capacity()];
        let mut iovecs: Vec<libc::iovec> = batch
            .buffers
            .iter_mut()
            .map(|buffer| libc::iovec {
                iov_base: buffer.as_mut_ptr().cast(),
                iov_len: buffer.len(),
            })
            .collect();
        let mut headers: Vec<libc::mmsghdr> = names
            .iter_mut()
            .zip(iovecs.iter_mut())
            .map(|(name, iovec)| libc::mmsghdr {
                msg_hdr: libc::msghdr {
                    msg_name: ptr::from_mut(name).cast(),
                    msg_namelen: SOCKADDR_LEN_C,
                    msg_iov: ptr::from_mut(iovec),
                    msg_iovlen: 1,
                    msg_control: ptr::null_mut(),
                    msg_controllen: 0,
                    msg_flags: 0,
                },
                msg_len: 0,
            })
            .collect();
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
        for (index, header) in headers.iter().take(received).enumerate() {
            let truncated = header.msg_hdr.msg_flags & libc::MSG_TRUNC != 0;
            let source = parse_sockaddr(&names[index][..]);

            let length = match (truncated, source) {
                (false, Some(source)) => {
                    batch.sources[index] = source;
                    self.unseal(&mut batch.buffers[index], header.msg_len as usize)
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
}
