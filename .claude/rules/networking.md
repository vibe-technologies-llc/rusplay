---
paths:
  - "crates/rusplay-net/**"
  - "crates/rusplay-protocol/**"
---

# Networking

- Media travels over raw UDP with Rusplay's own protocol. No QUIC and no TCP on the media
  path, so one lost packet never stalls the next frame.
- `rusplay-protocol` is pure data: header layout, message types, version. It does no I/O, so it
  can be tested byte for byte. `rusplay-net` owns sockets, pacing, FEC and reassembly.
- Every datagram starts with the 16-byte big-endian `PacketHeader`: version, stream kind, flags,
  sequence, frame number, fragment index and fragment count. `PacketHeader::read` rejects a
  fragment index at or beyond the count. A datagram never exceeds the configured MTU minus the
  IP and UDP headers (`max_datagram_len`: 28 bytes for IPv4, 48 for IPv6), so nothing relies on IP
  fragmentation.
- Any change to the header or message encoding bumps `ProtocolVersion::CURRENT`. Peers with
  different versions refuse each other with `VersionMismatch`, with no compatibility shims.
- Loss is handled per frame: Reed-Solomon FEC shards (`FecScheme`, overhead from
  `network.fec_overhead_percent`) recover small losses, and an unrecoverable frame triggers a
  `RequestKeyframe`/intra-refresh request instead of retransmission.
- FEC is `reed-solomon-simd` (pure Rust, Leopard codes). A frame's data fragments are split into
  balanced blocks of at most `data_shards` fragments (`FecScheme::blocks`), each block with its
  own parity shards (`parity_shards` scaled to the block size, at least one), so a large keyframe
  never exceeds the codec's shard limits. Shard lengths must be even and equal within a block.
- Frames are paced across the frame interval (`Pacer`) instead of sent in a burst, to keep switch
  buffers and the client's socket from dropping packets.
- The control channel (hello, keyframe requests, loss reports, bitrate hints) has its own small
  acknowledgement and retransmit scheme on the same socket, kept off the media path.

## Fragmentation and reassembly

- `Packetizer` (one per stream, owns the sequence counter) splits a frame into data fragments of
  one even `shard_len` (the datagram budget minus both headers, rounded down to even), all full
  except the last. Data packets carry the frame's stream kind, `fragment_index`/`fragment_count`
  over the data fragments, `KEYFRAME` on every packet of a keyframe and `LAST_FRAGMENT` on the
  final data fragment. The sequence counts every datagram of the stream, data and parity, and
  wraps.
- Parity packets use `StreamKind::Fec`, carry the same frame number, and are indexed over the
  frame's parity packets. Their payload starts with the 10-byte `FecHeader` (protected stream,
  first fragment of the block, block data and parity shard counts, parity index within the block,
  total frame bytes) so a receiver can decode from parity alone, learn the fragment count
  (`ceil(frame_bytes / shard_len)`) and truncate a recovered tail fragment to its true length. The
  last data fragment is zero-padded to `shard_len` only for coding, never on the wire.
- `Reassembler` (one per stream) validates before it stores: the first packet of a frame fixes
  the fragment count, shard length and frame size, and any later packet that contradicts them is
  rejected with a specific `NetError` and leaves the frame intact. A rejected packet of an unseen
  frame creates no state. Fragment count is capped by `ReassemblyLimits::max_fragments`.
- Latency first: when a frame completes, every older pending frame is reported
  `ReassemblyEvent::FrameLost` immediately and its late packets are dropped as stale. Pending
  frames are also lost when they exceed `max_pending_frames` or live longer than
  `network.reassembly_timeout_ms` (`poll_timeouts`, `next_timeout`). The caller answers a
  `FrameLost` with `ControlMessage::RequestKeyframe`.
- Frame numbers and sequences use serial-number arithmetic (`wrapping_sub` as `i32`), so wrap is
  transparent. A sender that restarts its frame numbering needs `Reassembler::reset`.
- `ReassemblyEvent::PacketsMissing` reports a sequence gap for `ControlMessage::ReportLoss`. A
  packet that arrives after its gap was reported was merely reordered, so loss reports are advisory.
