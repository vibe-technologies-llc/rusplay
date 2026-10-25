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
  sequence, frame number, fragment index and fragment count. Datagrams never exceed the
  configured MTU, so nothing relies on IP fragmentation.
- Any change to the header or message encoding bumps `ProtocolVersion::CURRENT`. Peers with
  different versions refuse each other with `VersionMismatch`, with no compatibility shims.
- The cursor travels on its own `StreamKind::Cursor` as `CursorMessage`: big-endian position, a
  flags byte (visible, has image) and, only when the shape changed, a `CursorImage` (u16 size and
  hotspot, then tightly packed RGBA8). It is latest-wins like video, never queued behind frames.
- Loss is handled per frame: Reed-Solomon FEC shards (`FecScheme`, overhead from
  `network.fec_overhead_percent`) recover small losses, and an unrecoverable frame triggers a
  `RequestKeyframe`/intra-refresh request instead of retransmission.
- Frames are paced across the frame interval (`Pacer`) instead of sent in a burst, to keep switch
  buffers and the client's socket from dropping packets.
- The control channel (hello, keyframe requests, loss reports, bitrate hints) has its own small
  acknowledgement and retransmit scheme on the same socket, kept off the media path.
