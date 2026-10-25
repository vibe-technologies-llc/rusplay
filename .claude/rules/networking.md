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
- The cursor travels on its own `StreamKind::Cursor` as `CursorMessage`: big-endian position, a
  flags byte (visible, has image) and, only when the shape changed, a `CursorImage` (u16 size and
  hotspot, then tightly packed RGBA8). It is latest-wins like video, never queued behind frames.
- Loss is handled per frame: Reed-Solomon FEC shards (`FecScheme`, overhead from
  `network.fec_overhead_percent`) recover small losses, and an unrecoverable frame triggers a
  `RequestKeyframe`/intra-refresh request instead of retransmission.
- FEC is `reed-solomon-simd` (pure Rust, Leopard codes). A frame's data fragments are split into
  balanced blocks of at most `data_shards` fragments (`FecScheme::blocks`), each block with its
  own parity shards (`parity_shards` scaled to the block size, at least one), so a large keyframe
  never exceeds the codec's shard limits. Shard lengths must be even and equal within a block.
- `Transport::bind` applies `network.socket_buffer_bytes` (through `TransportOptions`) to both
  `SO_SNDBUF` and `SO_RCVBUF` with `rustix` before any traffic flows; `0` leaves the kernel
  default. Linux clamps the request to `net.core.wmem_max`/`rmem_max` without an error, so
  `Transport::socket_buffers` reports the sizes actually in force for the caller to log or warn on.
- Fragments of a frame leave in one `sendmmsg` call (`Transport::send_batch`, through rustix;
  the whole batch is refused up front if any datagram exceeds the budget, and a short count means
  the socket buffer filled on a non-blocking socket, so the caller re-queues the tail).
  Receivers drain with one `recvmmsg` (`Transport::recv_batch` into a reusable `RecvBatch`,
  `MSG_WAITFORONE`, returns as soon as one datagram is in; `recv_batch_timeout` sets
  `SO_RCVTIMEO` first and returns 0 when nothing came). rustix has no `recvmmsg`, so that call
  is the crate's single `unsafe` block, over `libc`. Truncated datagrams are dropped from the batch.
- Frames are paced across the frame interval (`Pacer`) instead of sent in a burst, to keep switch
  buffers and the client's socket from dropping packets. The host spreads each frame over
  `network.pacing_window_percent` (default 50) of the interval, so pacing never delays a frame by
  more than that window; `network.pacing = false` sends each frame in one `sendmmsg` burst.
- `ControlMessage::Keepalive` (tag 5, no body) is sent by the host every
  `network.keepalive_interval_ms` (default 1000, validated below `network.peer_timeout_ms`). Its
  acknowledgement is the host's evidence that the client is alive, and its arrival keeps the
  client's peer timeout quiet while a static screen produces no video. Receivers ignore it apart
  from the acknowledgement.
- The control channel (hello, keyframe requests, loss reports, bitrate hints, keepalives,
  goodbye) has its own small acknowledgement and retransmit scheme on the same socket, kept off
  the media path.

## Control channel

- `ControlMessage` has a fixed big-endian encoding (tag byte, then fields; `decode` rejects short
  input, unknown tags and trailing bytes). A `ControlPacket` is a `StreamKind::Control` datagram:
  `sequence` is the message id, the `ACK` flag marks an acknowledgement (header only, same id),
  and a message datagram's payload is the encoded message. Ids are per direction.
- `ControlChannel` is sans-IO and owns no socket, thread or lock, so the media path never waits on
  it. The caller feeds it control datagrams (`handle_datagram`), drives timers (`poll`,
  `next_deadline`) and sends what it produced (`flush` on a non-blocking `Transport`, which keeps
  a datagram queued on `WouldBlock`, or `take_transmits`). Media code only ever sees the datagrams
  the caller routes to it by `StreamKind`.
- Delivery is at-most-once and unordered: every message datagram is acknowledged, retransmitted
  every `network.control_retransmit_ms` with exponential backoff (capped at 16x) up to
  `network.control_max_retransmits` times, then reported `ControlEvent::Undeliverable`. Receivers
  deduplicate with a 64-id window; the sender allows at most `CONTROL_WINDOW` (64) ids between its
  oldest unacknowledged message and the next id (`ControlWindowFull`), so a retransmit can never
  fall outside the receiver's window and be mistaken for a duplicate.
- Both ends start at id 0. A peer that restarts its session must `reset` its channel and the other
  end must `reset` too, otherwise the new ids look like old duplicates.

## Discovery

- `StreamKind::Discovery` carries `DiscoveryMessage`: a `Query` with a random nonce and an
  `Announce` echoing that nonce with the host's media port and a name of at most 64 UTF-8 bytes.
  Announces are unauthenticated hints; pairing, not discovery, establishes trust.
- `DiscoveryResponder` (host) listens on `network.discovery_port` on all IPv4 interfaces, joins
  `network.discovery_group`, and answers each query by unicast to its sender. `handle_next`
  serves one datagram with a timeout and is meant to run in a loop on its own thread.
  `DiscoveryBrowser` (client) sends the query to the limited broadcast address and the group,
  repeats it three times over the scan window, and returns the hosts whose announce echoed its
  nonce, deduplicated by address. Tests aim it at loopback addresses instead of broadcast.
- Discovery is IPv4 only. Limited broadcast leaves through the default-route interface only and
  the multicast group has TTL 1, so a host on another VLAN or a second interface is not found;
  entering an address by hand always works.

## Pairing and encryption

- Trust comes from a PIN the host shows and the user types on the client. The PIN runs through
  SPAKE2 (`spake2`, Ed25519 group, identities `rusplay client`/`rusplay host`), so a short
  PIN is never exposed to offline guessing. `PairingMessage` on `StreamKind::Pairing` carries
  `Start` (client key share), `Reply` (host share plus key confirmation), `Confirm` (client key
  confirmation), `Done` and `Rejected`, all tagged with a random per-attempt id.
- The SPAKE2 secret feeds HKDF-SHA256 (salted with the attempt id) into two confirmation keys and
  two ChaCha20-Poly1305 session keys, one per direction. Each side proves it derived the same
  secret (HKDF-based tags over the label, attempt and both key shares, compared in constant time);
  a wrong PIN fails on the client at `Reply` and on the host at `Confirm`.
- Online guessing is bounded by `network.pairing_max_attempts`: every fresh `Start` that gets a
  `Reply` costs one attempt, because the host's confirmation already lets a hostile client test one
  PIN. A retransmitted `Start` (same attempt id) is answered from cache for free. Once the budget
  is spent the host answers `Rejected(Locked)` and `pair_as_host` returns `PairingLocked`; a
  completed pairing consumes the PIN. The PIN has `network.pairing_pin_digits` digits and
  `Pin::generate` draws it from the OS random source without modulo bias.
- `PairingInitiator`/`PairingResponder` are sans-IO; `pair_as_client`/`pair_as_host` drive them over
  a blocking `Transport` with retransmits every `network.control_retransmit_ms` until
  `network.pairing_timeout_ms`, and the host lingers a few retransmit intervals after success to
  repeat `Done` to a client that missed it. Pairing is not on the media path.
- `Transport::install_session` switches a transport to encrypted mode. Every datagram except
  `Pairing` and `Discovery` is sealed as `PacketHeader` (authenticated as associated data), a
  big-endian 64-bit counter, the ciphertext and a 16-byte tag, 24 bytes of overhead that
  `max_datagram_len` already subtracts, so create packetizers after installing the session. The
  counter is the AEAD nonce and feeds a 64-entry replay window per direction. Unsealed, replayed or
  forged datagrams are dropped inside `recv_from`, `try_recv_from`, `recv_from_timeout` and
  `recv_batch` and only counted (`rejected_datagrams`), so an attacker cannot make the receive
  loops fail. Control, FEC and media all ride the sealed path unchanged.
- Without a session the transport stays plaintext, which is what tests and unpaired development
  use; refusing to stream before pairing is the session layer's policy. Key material is zeroized on
  drop and never printed by `Debug`.
- Known limits: a PIN is single use, and a LAN attacker can burn the attempt budget with garbage
  `Start`s, which only denies pairing and never grants access.
- A completed pairing also yields a long-term `PeerSecret` (`PairingOutcome::secret`, HKDF
  `rusplay peer secret` from the same SPAKE2 key), identical on both sides. Its `PeerId` is the
  first 16 bytes of SHA-256 over a domain label and the secret, so ids need no extra wire field.

## Remembered peers

- `PeerStore` is a plain list of `PeerRecord`s (id, name of at most 64 bytes without control
  characters, secret) used on both sides: the host's paired clients and the client's paired hosts.
  The file is one line per peer, `<id hex> <secret hex> <name>`; loading refuses a malformed line,
  an id that is not the secret's, and duplicates (`PeerStoreCorrupt` with the line number).
- Files live in `$XDG_STATE_HOME/rusplay/` (fallback `~/.local/state/rusplay/`, relative values
  ignored): `paired-clients` for the host, `paired-hosts` for the client. `peers.host_file` and
  `peers.client_file` override them (`PeerStore::open_host`/`open_client`). The directory is
  created `0700`; writes go to a freshly created `0600` staging file that is synced and renamed over
  the store, so a crash leaves the old file and a stale permissive staging file is never reused.
  `remember` and `forget` persist before they change the in-memory list, so a failed write changes
  nothing. `forget` is the revocation API; the host shares the store as
  `RwLock<PeerStore>` so a forget applies to the next datagram.
- Reconnect is a mutual challenge-response on `StreamKind::Reconnect` (unsealed, like pairing):
  `Hello` (random attempt id, `PeerId`, 32-byte client nonce), `Challenge` (host nonce plus the
  host proof), `Proof` (client proof), `Accepted` and `Rejected` (`UnknownPeer`, `Authentication`).
  Both proofs and both direction keys are HKDF-SHA256 outputs keyed by the secret and salted with
  peer id, attempt and both nonces, each under its own label, so a proof proves knowledge of the
  secret and freshness from both sides, a reflected message never verifies, and session keys are
  never reused across reconnects. The client checks the host proof before it sends its own.
- The secret is 256 bits, so reconnect has no attempt budget. `ReconnectResponder` keeps the last 16
  attempts (retransmitted `Hello`/`Proof` are answered from cache), re-checks the store when the
  `Proof` arrives, and keeps an attempt alive after a bad `Proof` so a forged one cannot spoil the
  genuine client's attempt. An unknown or forgotten id is `Rejected(UnknownPeer)`; the client then
  falls back to PIN pairing.
- `reconnect_as_client`/`reconnect_as_host` mirror the pairing drivers, bounded by
  `network.reconnect_timeout_ms`; the host lingers after success to repeat `Accepted`.

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
