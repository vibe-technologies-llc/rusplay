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
- The cursor travels on its own `StreamKind::Cursor` as `CursorMessage`: big-endian position, the
  `space` (u32 width and height) the position, hotspot and image are measured in, a
  flags byte (visible, has image) and, only when the shape changed, a `CursorImage` (u16 size and
  hotspot, then tightly packed RGBA8). It is latest-wins like video, never queued behind frames.
  The receiver cannot ask for a lost image, so the host repeats it every
  `network.cursor_image_repeat_ms`.
- The client's microphone travels on `StreamKind::Microphone` exactly like host audio: one Opus
  packet per frame number counting up from 0, through a `Packetizer` (FEC optional, `Fec`
  datagrams name it in `FecHeader::protects`), reassembled by the host's microphone stage. It is
  only sent client to host.
- Input travels on `StreamKind::Input` as `InputPacket`: the header `sequence` counts datagrams
  and the payload is a big-endian `u16` length of the `current` section, the `current` section and
  the `previous` section (the preceding datagram's `current`), each a run of `[len u8][record]`.
  The records are opaque to the protocol crate (`rusplay-input`'s `InputRecord` bytes: an
  `InputEvent`, or a `StateRecord` of the periodic held-state snapshot).
  `InputSequence::admit` tells the receiver whether to drop a datagram, apply `current`, or apply
  `previous` and `current`, in serial-number arithmetic. Input is unreliable on purpose, with
  that one-datagram redundancy and the periodic snapshot as its only loss repair. See `input.md` for the full contract.
- Loss is handled per frame: Reed-Solomon FEC shards (`FecScheme`, overhead from
  `network.fec_overhead_percent`) recover small losses, and an unrecoverable frame triggers a
  `RequestRecovery` or `RequestKeyframe` instead of retransmission (below).
- `ControlMessage::AcknowledgeFrame { frame }` (tag 10, client to host, 5 bytes) names the newest
  frame the client's decoder decoded on an unbroken reference chain; the host hands it to the
  encoder as a confirmed reference candidate. `ControlMessage::RequestRecovery { frame, last_good }`
  (tag 11, client to host, 9 bytes) asks for a refresh after the lost `frame` that references
  nothing newer than `last_good`, the client's last good frame; the host answers with a recovery
  frame or, when its encoder cannot, a keyframe. Both are big-endian u32s after the tag.
  `RequestKeyframe { frame }` stays for a client with no good frame.
- `ControlMessage::BitrateHint { kbps }` (tag 3, client to host, 5 bytes) asks the host's encoder
  for an absolute bitrate, and `ControlMessage::BitrateCeiling { kbps }` (tag 16, host to client,
  5 bytes) is the most a hint can ask for: the bitrate the host's encoder is configured for (the
  host's `video.bitrate_kbps`), with 0 meaning the encoder has no bitrate to steer (constant
  quality). Both are a big-endian u32 of kbps after the tag, and a short or long body is rejected
  like every other fixed message. The two role configs are separate files, so a client that
  knew only its own `video.bitrate_kbps` would steer toward that: a host configured for more
  would be held to the client's lower rate after the first loss for the rest of the session,
  and a host configured for less would be raised above what it was told to send. The host
  sends the ceiling right after it answers a `Hello` (`host.md`) and clamps every hint to it;
  the client starts its controller from it (`client.md`). Delivery is unordered, so the
  client takes it whenever it arrives and re-bases on a different value. `ProtocolVersion::CURRENT`
  28 added it; the `control` fuzz target forces its tag in front of arbitrary bytes with the other
  fixed layouts.
- FEC is `reed-solomon-simd` (pure Rust, Leopard codes). A frame's data fragments are split into
  balanced blocks of at most `data_shards` fragments (`FecScheme::blocks`), each block with its
  own parity shards (`parity_shards` scaled to the block size, at least one), so a large keyframe
  never exceeds the codec's shard limits. Shard lengths must be even and equal within a block.
- `Transport::bind` applies `network.socket_buffer_bytes` (through `TransportOptions`) to both
  `SO_SNDBUF` and `SO_RCVBUF` with `rustix` before any traffic flows; `0` leaves the kernel
  default. Linux clamps the request to `net.core.wmem_max`/`rmem_max` without an error, so
  `Transport::socket_buffers` reports the sizes actually in force for the caller to log or warn on.
- Fragments of a frame leave in one `sendmmsg` call (`Transport::send_batch`, through rustix;
  the whole batch is refused up front if any datagram exceeds the budget). Every `sendmmsg` passes
  `MSG_DONTWAIT`, so a full send buffer returns a short count instead of blocking the thread and
  the caller re-queues the tail: the host's send stage waits 200 µs (`SEND_RETRY_WAIT`) and
  checks its stop flag and the suspension count before it retries, so a wedged interface never
  holds `HostStream::stop` in `join`. The socket itself stays blocking, because the receive
  timeouts are `SO_RCVTIMEO`. The video
  path uses `Transport::send_arena` instead, which takes a `DatagramArena` and an index range (see
  Fragmentation).
  Receivers drain with one `recvmmsg` (`Transport::recv_batch` into a reusable `RecvBatch`,
  `MSG_WAITFORONE`, returns as soon as one datagram is in; `recv_batch_timeout` sets
  `SO_RCVTIMEO` first, only when it differs from the value the transport last applied
  (`recv_from_timeout` shares that cache), and returns 0 when nothing came). The `RecvBatch` owns
  the name, iovec and `mmsghdr` arrays and re-arms them in place on every call, so a receive
  allocates nothing; it holds raw pointers while armed and is therefore not `Send`. rustix has no `recvmmsg`, so that call
  is the crate's single `unsafe` block, over `libc`. Truncated datagrams are dropped from the batch.
- Frames are paced (`Pacer`) instead of sent in one burst, to keep switch buffers and the
  client's socket from dropping packets, and a frame is only decodable once its last datagram is
  in, so the pacing span is latency on every frame. The host sends a frame at
  `network.pacing_bitrate_multiple` (default 4, 0 off) times the stream bitrate in force (the
  configured bitrate, then the last applied hint), so a frame of average size takes a quarter of
  the frame interval (4.2 ms at 60 fps against 8.3 for the whole window), a small frame
  proportionally less, and a static desktop's few datagrams leave at once. The span never
  exceeds `network.pacing_window_percent` (default 50) of the interval, which is what a larger
  frame gets and what every frame gets when the multiple is off or the encoder has no bitrate
  (constant quality). Four times the bitrate is no faster than the window already sends a
  keyframe: with `video.vbv_frames` 2 a keyframe is up to two or three average frames in half
  an interval, four to six times the bitrate, so a link that carries the keyframes carries
  this, and a link that loses packets lowers the rate with the bitrate through the client's
  hints. `network.pacing_max_mbps` (default 0, off) is an absolute rate for a link that was
  measured: alone it paces every frame at that rate, with the multiple it caps it.
  `network.pacing = false` sends each frame in one `sendmmsg` burst.
- Every video frame handed to the `Packetizer` starts with the 13-byte big-endian
  `VideoFrameHeader`: the capture time in microseconds on the host's stream clock (zero at session
  start, `rusplay-host`'s `StreamClock`), a flags byte (bit 0 recovery; other bits are
  `UnknownVideoFrameFlags`) and a u32 reference frame number. A recovery frame
  (`recovers_from: Some(reference)`) is a non-keyframe that references only frame `reference` or
  older frames the client decoded before it, so a client that decoded `reference` can resume on
  it; without the flag the reference must be zero (`ReferenceWithoutRecovery`). It is part of the
  frame, so the first fragment carries it and FEC covers it. The receiver strips it after reassembly and treats a frame shorter than the
  header as malformed (counted, answered with a keyframe request).
- `ControlMessage::ClockPing { sent_micros }` (tag 8, client to host) and
  `ClockPong { sent_micros, host_micros }` (tag 9, host to client) are an NTP-style exchange on the
  control channel: the client stamps its own clock, the host echoes it with its stream clock at
  handling time, and the client takes `offset = host - (sent + received) / 2` from the sample with
  the smallest round trip among the last 8, so a retransmitted or queued sample (large round trip)
  never wins. The offset is correct to within half the path asymmetry, which on a LAN is well below
  the latencies it measures. The client pings every 100 ms until the window is full, then every
  `network.clock_sync_interval_ms` (default 1000, 0 turns the exchange and the latency figures
  off). It runs on the receive thread's control channel, never on the frame path.
- `ControlMessage::Keepalive` (tag 5, no body) is sent by the host every
  `network.keepalive_interval_ms` (default 1000, validated below `network.peer_timeout_ms`). Its
  acknowledgement is the host's evidence that the client is alive, and its arrival keeps the
  client's peer timeout quiet while a static screen produces no video. Receivers ignore it apart
  from the acknowledgement.
- `ControlMessage::StreamFormat(StreamDescription)` (tag 7) is how the host describes the
  stream: a `generation` (u32, serial arithmetic, `is_newer_than`), codec, resolution, pixel
  format (chroma tag, bit depth in bits) and the full `ColorSpace`. Colour codes are H.273:
  primaries code (0 means custom, followed by red, green, blue and white as big-endian u16
  0.00002 steps), transfer code, matrix code (2 means unset, so the receiver derives it from the
  primaries), range byte, reference white and peak as u32 nits, then a flags byte (bit 0
  mastering display, bit 1 content light; other bits are `UnknownHdrFlags`) and, when flagged,
  the mastering primaries in the same form plus max and min luminance as u32 0.0001 cd/m²
  units, and MaxCLL/MaxFALL as u16, then the `configured` `CodecFormat` (codec tag, chroma
  tag, depth bits): the host's configured format, so `is_fallback` tells the client when the
  stream is a negotiated fallback (format negotiation, below), and last the chroma `siting` byte
  (0 left, 1 centre, other values are `UnknownChromaSiting`): the `scaling.chroma_siting` the host
  converted with, which the client uses when the bitstream cannot say (AV1 has no centred
  horizontal position). Variable length, at most 78 bytes
  with its tag (`ControlMessage::MAX_ENCODED_LEN`). Decoding validates everything it reads (custom primaries
  through `Chromaticities::new`, luminance through `Luminance::from_units`, each resolution axis in
  `1..=StreamDescription::MAX_DIMENSION` (16384), reference white and peak in `1..=10000` nits and
  mastering minimum below maximum), each failure its own `ProtocolError`. Delivery is unordered, so receivers keep only a newer generation.
- `ControlMessage::Hello { version, formats }` (tag 0, 7 bytes): the u16 protocol version, then
  a big-endian u32 `FormatSet` (`rusplay-core`), one bit per `CodecFormat` at
  `codec * 9 + chroma * 3 + depth` (codec H.264, HEVC, AV1; chroma 4:2:0, 4:2:2, 4:4:4; depth 8,
  10, 12). Bits 27 and up are `UnknownFormatBits`. The client sends what its decoder decodes,
  the host answers with what its encoder produces.
- `ControlMessage::Display(ClientDisplay)` (tag 15, client to host, 27 to 43 bytes with the tag):
  the client's display, sent right after its `Hello` whatever the host's source, so no host depends
  on the order the two arrive in. Big-endian u32 width and height (each `1..=StreamDescription::MAX_DIMENSION`,
  `DisplayResolutionOutOfRange`), u32 refresh in millihertz (`1000..=1_000_000`,
  `DisplayRefreshOutOfRange`; `ClientDisplay::frame_rate` reduces it to a `FrameRate`), then the
  colour volume: primaries (the `StreamDescription` encoding, custom ones followed by their
  chromaticities), transfer code, reference white and peak as u32 nits (`1..=10000`, the
  stream description's errors) and black as u32 0.0001 cd/m² units, which must stay below the
  reference white (`DisplayBlackNotBelowWhite`). `ClientDisplay::new` is the only constructor and
  `decode` goes through it, so a value in hand is always in range. The host ignores it unless
  `capture.source` is `virtual` (`host.md`). The client sends it again whenever its stream area
  changes, once the new size has settled (`client.md`); the encoding did not change, so
  `ProtocolVersion::CURRENT` did not either, and a host that predates the later sends keeps the
  last one it received and uses only the first. Control delivery is unordered, so the client keeps
  one `Display` in flight and sends the newest after the acknowledgement. The host applies the
  newest pending `Display` at most once per `capture.virtual_resize_min_interval_ms`.
  `rusplay-fuzz`'s `control` target covers it through `ControlMessage::decode`.
- Format negotiation: the host picks
  `FormatSet::best_for(preference)` from encodable ∩ decodable, where the preference comes from
  the host's `video.codec` and `video.pixel_format`; automatic axes rank after pinned ones
  (`auto.md`). For pinned axes the fallback order is lexicographic: codec
  first, then chroma, then depth. On the chroma and depth axes the configured value comes first,
  then higher values nearest first (nothing asked for is lost), then lower values nearest first;
  codecs are the configured one, then AV1, HEVC, H.264. The configured codec is kept whenever
  both ends support any of its formats, even if another codec matches the configured pixel format
  more closely. HEVC 4:2:2 8-bit on a radeonsi host (HEVC 4:2:0 only) becomes HEVC 4:2:0 8-bit.
  An empty intersection is `NoCommonFormat { encodable, decodable }` on both ends: the host
  still answers the `Hello` with its set before its `Goodbye`, and the client computes the same
  empty intersection from it. The order is unit tested in `rusplay-core/src/format.rs`.
- The control channel (hello, keyframe and recovery requests, frame acknowledgements, loss
  reports, bitrate hints and the bitrate ceiling, keepalives, stream format, clock ping and pong, clipboard text, goodbye) has its own small acknowledgement and retransmit scheme on the same socket, kept off
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
- `ControlMessage::Rumble { slot, strong, weak, delay_ms, duration_ms }` (tag 6, host to client, 10
  bytes) carries gamepad force feedback; reliable delivery keeps a stop from being lost, and the
  client caps every effect length so reordering cannot leave a motor running.
- Pad feedback from a DualSense personality pad rides the same reliable channel, host to client:
  `ControlMessage::Lightbar { slot, red, green, blue }` (tag 12, 5 bytes), `PlayerLeds { slot,
  mask }` (tag 13, 3 bytes, bit 0 the first LED, only the low five bits) and `TriggerEffect {
  slot, right, effect }` (tag 14, 14 bytes: the slot, a side byte that must be 0 for left or 1 for
  right (`UnknownTriggerSide` otherwise) and the 11 effect bytes of the output report, mode first,
  opaque to the protocol). `decode_pad_feedback` decodes these three and rejects a short or long
  body like every other message; the `control` fuzz target round-trips whatever decodes and also
  forces each of the three tags in front of arbitrary bytes so the new decoder is exercised from
  the first input. `ProtocolVersion::CURRENT` 24 added them. They are not idempotent against
  reordering in the way a stop is, but each carries the whole state of its LED or trigger, so a
  late one only repeats an older state until the next.
- Clipboard text rides the same reliable channel as `ControlMessage::Clipboard(ClipboardChunk)`
  (tag 20, host to client or client to host): a text never fits one datagram, so a transfer is
  `count` chunks sharing a `transfer` number (u32, serial arithmetic). A chunk is
  `[transfer u32][index u16][count u16][len u16][bytes]`, big-endian, at most
  `ClipboardChunk::MAX_DATA` (192) bytes, so with the headers and the seal it fits the smallest
  MTU (576). Decoding enforces `1 <= count`, `index < count`, a full 192 bytes on every chunk but
  the last, 1 to 192 on the last, and an exact body length, each failure its own `ProtocolError`.
  `ClipboardReassembler::new(max_bytes)` refuses a transfer whose chunk count already implies more
  than `max_bytes` (`ClipboardTooLarge`) before allocating, keeps one pending transfer (a newer
  number replaces it, an older or finished one is ignored), refuses a chunk that changes the
  count mid-transfer, and on completion refuses text over the limit or not valid UTF-8
  (`ClipboardNotUtf8`). `ClipboardChunk::split` refuses empty text. `ProtocolVersion::CURRENT` 26
  added it; `ControlMessage::MAX_ENCODED_LEN` is now set by the chunk (the chunk size keeps `ControlMessage`
  under clippy's 256 byte by-value limit; `ControlPacket` carries `#[allow(clippy::large_enum_variant)]`
  for its header-only `Ack`). The `clipboard`
  fuzz target decodes arbitrary bodies, round-trips what decodes and feeds the reassembler. A sender
  with more chunks than the 64-id window leaves the rest queued until acknowledgements free ids
  (`clipboard.md`).
- Quality metrics (`quality.rs`) ride the reliable channel: `ControlMessage::QualityRequest {
  enabled }` (tag 21, client to host, 2 bytes, the flag 0 or 1, `UnknownQualityRequest`
  otherwise) turns the host's sampling on or off for the session; `QualityReport(QualityReport)`
  (tag 22, host to client, 19 bytes) is the frame number as a u32 then PSNR Y, U, V, overall and
  XPSNR Y, U, V as big-endian u16 hundredths of a decibel (`QualityDecibels`, clamped to
  `0..=65534`, `u16::MAX` meaning lossless); `QualityUnavailable { reason }` (tag 23, host to
  client, 2 bytes: 0 encoder unsupported, 1 host disabled, 2 measurement failed,
  `UnknownQualityUnavailableReason` otherwise). Short and long bodies are rejected like every
  other message, and the `control` fuzz target forces the three tags in front of arbitrary bytes.
  `ProtocolVersion::CURRENT` 27 added them.
- `ControlChannel` keeps a smoothed RTT (`rtt`, `rtt_variation`; RFC 6298: the first sample seeds
  it, later ones use gains 1/8 and 1/4) from the time a message was sent to its acknowledgement.
  A message that was retransmitted never samples (Karn), so an ambiguous ack cannot skew it.
  `handle_datagram` takes the arrival `now` for that. The host and client control stages publish
  it as `HostStats::rtt` / `ClientStats::rtt` (`None` until the first unretransmitted ack, and
  after a `reset`).
- Both ends start at id 0. A peer that restarts its session must `reset` its channel and the other
  end must `reset` too, otherwise the new ids look like old duplicates.

## Discovery

- `StreamKind::Discovery` carries `DiscoveryMessage`: a `Query` with a random nonce and an
  `Announce` echoing that nonce with the host's media port, a flags byte (bit 0 `busy`, any
  other bit is `UnknownDiscoveryFlags`), the host's 16-byte `HostId` (all zero, `HostId::NONE`,
  when the responder has none) and a name of at most 64 UTF-8 bytes.
  Announces are unauthenticated hints; pairing, not discovery, establishes trust.
- `DiscoveryResponder` (host) listens on `network.discovery_port` on all IPv4 interfaces, joins
  `network.discovery_group`, and answers each query by unicast to its sender. `handle_next`
  serves one datagram with a timeout and is meant to run in a loop on its own thread.
  `with_busy_flag` hands it a shared `AtomicBool` it reads for every reply, which
  `DiscoveredHost::busy` reports on the client.
  `with_host_id` sets the id, `DiscoveredHost::id` reports it. `load_or_create_host_id(path)`
  reads a stable random id from a file or, when it does not exist, draws one from the OS random
  source and writes it owner only (`write_owner_only`); a file that is not 32 hex digits, or is
  all zero, is `HostIdCorrupt` and is never overwritten.
  `DiscoveryBrowser` (client) sends the query to the limited broadcast address and the group,
  repeats it three times over the scan window (`network.discovery_scan_ms`, default 1000), and
  returns the hosts whose announce echoed its nonce, deduplicated by address. Tests aim it at
  loopback addresses instead of broadcast.
- Discovery is IPv4 only and reaches every up, non-loopback interface (`ipv4_links`, from
  `if-addrs`): `DiscoveryBrowser::with_options` adds each interface's directed broadcast to the
  limited broadcast and the group (`DiscoveryOptions::link_targets`, deduplicated; a
  point-to-point link has none) and also sends the multicast query out of each interface
  (`IP_MULTICAST_IF` per send, reset afterwards), and the responder joins the group on every
  interface address, falling back to the default interface only when none joins. Limited
  broadcast alone would leave through the default-route interface. The group keeps TTL 1, so a
  host on a routed VLAN the client has no interface on is still not found; entering an address by
  hand always works. Pairing is untouched. Only the target computation and the link filter are
  unit tested: the namespaces a live test needs (`veth`) were not available on the development
  machine (`net-discovery-multi-interface-live-check`).

## Pairing and encryption

- Trust comes from a PIN the host shows and the user types on the client. The PIN runs through
  SPAKE2 (`spake2`, Ed25519 group, identities `rusplay client`/`rusplay host`), so a short
  PIN is never exposed to offline guessing. `PairingMessage` on `StreamKind::Pairing` carries
  `Start` (client key share), `Reply` (host share plus key confirmation), `Confirm` (client key
  confirmation), `Done` and `Rejected`, all tagged with a random per-attempt id.
- `Start` also carries the client's display name, `ClientName` (`rusplay-protocol`): a `u8`
  length then at most 64 bytes of UTF-8 without control characters, empty allowed. Decoding
  validates it, each failure its own `ProtocolError`. It is not part of the SPAKE2 transcript or
  the confirmations, so it is an unauthenticated display hint: the host stores it as the
  `PeerRecord` name and never uses it for identity (`PeerId` is the identity). The responder
  hands it to the caller as `PairingOutcome::client_name`. The client sends `peers.client_name`,
  else the kernel hostname (`rusplay-client/src/name.rs`), cut to 64 bytes on a character
  boundary with control characters dropped.
- The SPAKE2 secret feeds HKDF-SHA256 (salted with the attempt id) into two confirmation keys and
  two ChaCha20-Poly1305 session keys, one per direction. Each side proves it derived the same
  secret (HKDF-based tags over the label, attempt and both key shares, compared in constant time);
  a wrong PIN fails on the client at `Reply` and on the host at `Confirm`. A wrong `Confirm` is
  answered `Rejected(WrongPin)` but leaves the attempt in place, as a bad reconnect `Proof` does,
  because the attempt id travels in clear and one forged datagram must not end the genuine pairing
  in flight; the confirmation is fixed by the client's key share, so repeating it grants no
  further guess. The initiator and `reconnect_as_client` still end at once on a `Rejected` from the
  host address: a rejection cannot be authenticated before keys exist, a spoofer who can forge one
  could equally drop the traffic, and ignoring it would only delay `Busy` and `Locked` feedback
  to the user until the timeout.
- Online guessing is bounded by `network.pairing_max_attempts`: every fresh `Start` that gets a
  `Reply` costs one attempt, because the host's confirmation already lets a hostile client test one
  PIN. A retransmitted `Start` (same attempt id) is answered from cache for free. Once the budget
  is spent the host answers `Rejected(Locked)` and `pair_as_host` returns `PairingLocked`; a
  completed pairing consumes the PIN. The PIN has `network.pairing_pin_digits` digits and
  `Pin::generate` draws it from the OS random source without modulo bias.
- `PairingResponder::handle` takes the datagram's source address and binds each attempt to the
  address its `Start` came from: a `Confirm` or a retransmitted `Start` from any other address is
  ignored, so a replay of the unsealed final message from elsewhere cannot make the host install
  keys for the wrong peer.
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
  loops fail. Control, FEC and media all ride the sealed path unchanged. Only a datagram of a
  sealed stream kind from the peer's address (`proves_peer_liveness`) refreshes the host's and
  the client's peer timer: an unsealed `Discovery`, `Pairing` or `Reconnect` datagram passes the
  transport but proves nothing, so a spoofed source address cannot hold a session open.
- Without a session the transport stays plaintext, which is what tests and unpaired development
  use; refusing to stream before pairing is the session layer's policy. Key material is zeroized on
  drop and never printed by `Debug`.
- Busy host: `busy_reply` turns a pairing `Start` or a reconnect `Hello` into `Rejected(Busy)`
  under the same attempt id. The host's control stage sends it to any address but the current
  client's while a session runs, so a second client fails at once with `PairingRejected` or
  `ReconnectRejected` instead of timing out. The reply is shorter than the request, is sent
  best effort (a send failure never ends the session) and is never sent to a datagram that is
  not a first handshake message. A reconnect `Hello` blinded by the running session's own
  peer is the exception (below).
- Session takeover: `SessionTakeover` (sans-IO, per running session) wraps a
  `ReconnectResponder` for the session's `PeerId` and checks a `Hello`'s blind against that one
  record only (one HMAC, however large the store). A `Hello` blinded by any other secret, known
  peer or not, is `Busy`, and so is the session peer's own once it is forgotten (its secret is
  gone, so its blind is unrecognisable); a `Hello` from the session's peer runs the full mutual
  challenge-response, from whichever address it arrives (a restarted client has a new port, a
  roaming one a new IP). The cookie binds the `Proof` to the address that got the `Challenge`,
  so a sniffed proof replayed from elsewhere changes nothing; a source address alone is never
  trusted. Only the final `Accepted` yields keys, and only then does the host act (`host.md`). A
  forged proof is `Rejected(Authentication)` and leaves the session as it was. Takeover stays
  within one address family, since the packetizers are sized for it.
- Known limits: a PIN is single use, and a LAN attacker can burn the attempt budget with garbage
  `Start`s, which only denies pairing and never grants access.
- A completed pairing also yields a long-term `PeerSecret` (`PairingOutcome::secret`, HKDF
  `rusplay peer secret` from the same SPAKE2 key), identical on both sides. Its `PeerId` is the
  first 16 bytes of SHA-256 over a domain label and the secret. It never travels on the wire.

## Remembered peers

- `PeerStore` is a plain list of at most `MAX_PEERS` (256) `PeerRecord`s (id, name of at most 64
  bytes without control characters, secret) used on both sides: the host's paired clients and the
  client's paired hosts. The cap bounds the work of matching a reconnect blind (below): `remember`
  refuses a new peer when full (`PeerStoreFull`, re-remembering a known one still works) and a file
  with more lines fails to load (`PeerStoreTooLarge`).
  The file is one line per peer, `<id hex> <secret hex> <name>`; loading refuses a malformed line,
  an id that is not the secret's, and duplicates (`PeerStoreCorrupt` with the line number).
- Files live in `$XDG_STATE_HOME/rusplay/` (fallback `~/.local/state/rusplay/`, relative values
  ignored): `paired-clients` for the host, `paired-hosts` for the client. `peers.host_file` and
  `peers.client_file` override them (`PeerStore::open_host`/`open_client`). The directory is
  created `0700`, and an existing default state directory owned by the user is tightened to
  `0700` when the store opens (a directory named through `peers.host_file`/`peers.client_file` is
  never touched). `load` zeroizes the text holding the hex secrets and
  `PeerStore::exposure` reports a store file that is group or world readable or owned by another
  user (`StoreExposure`), for the caller to warn on; `rusplay-hostd` prints it. Writes go to a
  freshly created `0600` staging file that is synced and renamed over the store, so a crash leaves the old file and a stale permissive staging file is never reused.
  `remember` and `forget` persist before they change the in-memory list, so a failed write changes
  nothing. `forget` is the revocation API; the host shares the store as
  `RwLock<PeerStore>` so a forget applies to the next datagram. Another process can edit the
  file (`rusplay-hostd peers forget`): `PeerStore::is_stale` compares the file's modified time,
  length and inode with those recorded at the last load or write, and `refresh` re-reads when
  they differ, keeping the old records if the new file is corrupt.
  The write path is `write_owner_only`, shared with the client's host book (`client.md`).
- Reconnect is a mutual challenge-response on `StreamKind::Reconnect` (unsealed, like pairing):
  `Hello` (random attempt id, 32-byte client nonce, 32-byte blind), `Challenge` (32-byte cookie
  plus the host proof), `Proof` (the `Hello`'s nonce and blind, the cookie, the client proof),
  `Accepted` and `Rejected` (`UnknownPeer`, `Authentication`, `Busy`).
- Blinding: the `PeerId` never travels. The blind is HMAC-SHA256 under the peer secret over a
  label, the attempt id and the client nonce, so two `Hello`s of one device share nothing a passive
  observer can link. The host finds the record by recomputing the blind for each store record
  (constant-time compare, at most `MAX_PEERS` HMACs per `Hello` or `Proof`). No match is
  `Rejected(UnknownPeer)`, so a wrong secret and an unknown peer look alike.
- Stateless challenge: `ReconnectResponder` holds a random per-process cookie key and creates no
  per-attempt state for a `Hello`. The cookie is HMAC-SHA256 under that key over a label, the
  cookie period index, the source address (family, IP, port), the attempt, the client nonce and
  the blind; it doubles as the host nonce. A period is `network.reconnect_timeout_ms` counted
  from the responder's creation, and a `Proof` is honoured only while its cookie matches the
  current or the previous period (so it lives one to two periods) and only from the address it
  was issued to; anything else is ignored without a reply. The `Proof` carries everything the
  host needs to recompute the cookie, find the record and derive the schedule. A retransmitted
  `Hello` gets the same `Challenge` within a period, and the `Challenge` is no larger than the
  `Hello`, so a reflected one is no amplification. A flood of `Hello`s from any number of
  spoofed addresses therefore costs only CPU and can never displace a genuine attempt.
- Both proofs and both direction keys are HKDF-SHA256 outputs keyed by the secret and salted with
  peer id, attempt, client nonce and cookie, each under its own label, so a proof proves knowledge
  of the secret and freshness from both sides, a reflected message never verifies, and session
  keys are never reused across reconnects. The client checks the host proof before it sends its
  own.
- The secret is 256 bits, so reconnect has no attempt budget, and a bad `Proof` is answered
  `Rejected(Authentication)` without touching anything. The only state is created after a valid
  `Proof`: up to 64 acceptances (attempt, cookie, address, the `Accepted` datagram), kept until
  the cookie can no longer verify, so a retransmitted `Proof` is answered `Accepted` again
  without new keys. Only peers that proved the secret can fill it. A peer forgotten between
  `Hello` and `Proof` is `Rejected(UnknownPeer)`; the client then falls back to PIN pairing.
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
- `packetize_prefixed(frame, keyframe, prefix, body)` packetizes `prefix || body` without joining
  them: fragments and Reed-Solomon shards borrow from the two slices and only a shard that spans
  the seam (the first) or needs padding (the last) is copied, into one scratch buffer the
  `Packetizer` keeps, and its output is byte for byte that of `packetize` on the joined buffer (a
  test holds it). Parity comes from the `Packetizer`'s own `FecEncoder`, a reused
  `reed_solomon_simd::ReedSolomonEncoder` (`begin(block, shard_len)` resets it, `add` takes each
  shard, `finish` yields the recovery shards as slices) whose shards are copied straight into the
  arena slots, so no parity `Vec` exists; a test holds the parity equal to
  `reed_solomon_simd::encode` on the padded shards. The host passes the 8-byte
  `VideoFrameHeader` as the prefix, so a frame is never copied into a joined buffer.
  `send_batch` reads the session cipher once per batch, not once per datagram.
- `packetize_into(&mut DatagramArena, frame, keyframe, prefix, body)` is the allocation-free path
  (`packetize` and `packetize_prefixed` are wrappers that return plain `Vec`s from a throwaway
  arena). It clears the arena and writes every datagram into one reused buffer, each slot laid
  out as `PacketHeader`, 8 reserved counter bytes, body (`FecHeader` and payload for parity), 16
  reserved tag bytes. `Transport::send_arena(arena, range, peer)` seals the range in place
  (`SessionCipher::seal_in_place` fills the counter and tag, once per slot however often a range is
  retried) and hands `sendmmsg` the slot as one iovec, or, when unsealed (no session, or a stream
  that is not sealed), the header and body as two iovecs that skip the reserved gap. The wire bytes
  are identical to `send_batch`'s. The arena's storage is never shrunk or re-zeroed between
  frames, so a steady stream allocates no datagram or parity storage after the largest frame; the
  one copy left is the encoder's bytes into the arena. `SessionCipher::seal` is the same code on a freshly built slot.
- `Reassembler` (one per stream) validates before it stores: the first packet of a frame fixes
  the fragment count, shard length and frame size, and any later packet that contradicts them is
  rejected with a specific `NetError` and leaves the frame intact. A rejected packet of an unseen
  frame creates no state. A pending frame owns a recycled buffer of
  `fragment_count * shard_len`, sized once the count and shard length are known (the first
  non-final data fragment or any parity packet); fragments are written at `index * shard_len` and
  FEC decodes straight from those slots, so assembly truncates the buffer instead of copying. A
  final fragment that arrives before the shard length is known waits in a side buffer until it is.
  Completed frames return their storage through `FrameBytes` to a bounded channel on drop. Only
  the final shard padding is cleared on reuse; FEC reads only received or recovered slots.
  Parity shards share a slab per block. A frame that completes, is lost or is reset hands its
  received flags, block list and blocks (shard index and slab) to a spare pool in the
  `Reassembler`, bounded by `max_pending_frames` lists and `max_fragments` blocks, and the next
  frame takes them back, so steady FEC traffic allocates no parity storage. Paced sends use fixed stack arrays for syscall metadata.
  Fragment count is capped by `ReassemblyLimits::max_fragments`
  (`network.reassembly_max_fragments`, default 65535, the most the wire format can number) and
  the pending frames by `network.reassembly_max_pending_frames` (default 8). A sender that lowers
  the fragment cap must give its `Packetizer` the same number through `with_max_fragments`, which
  then returns `FrameTooLarge` above it, so a frame is refused at the sender rather than rejected
  packet by packet at the receiver.
- Latency first: when a frame completes, every older pending frame is reported
  `ReassemblyEvent::FrameLost` immediately and its late packets are dropped as stale. Pending
  frames are also lost when they exceed `max_pending_frames` or live longer than
  `network.reassembly_timeout_ms` (`poll_timeouts`, `next_timeout`). The caller answers a
  `FrameLost` with `ControlMessage::RequestRecovery` or `RequestKeyframe` (`client.md`).
- A frame is also lost as soon as its sender is known to be done with it, without waiting for
  the next frame to complete. Every sender (the host's video, audio and cursor stages, the
  client's microphone) owns one `Packetizer` on one thread and emits a frame's datagrams in
  packetizer order, all data fragments and then all parity, before it starts the next frame; a
  frame it abandons half sent is never resumed. A `Packetizer` keeps one `FecScheme` for its
  life, so either every frame of a stream has parity (at least one packet per block) or none
  has. The `Reassembler` records on a pending frame the first instant (`sender_done`) at which
  it accepted one of:
  - the frame's last parity packet: the `Fec` packet whose header index is the last of its
    count. Parity is numbered across the frame's blocks in block order, so that is the last
    packet of the last block and never the last packet of an earlier block;
  - the frame's last data fragment, on a stream that has already resolved a frame and has never
    carried a parity packet. A stale parity packet counts as carried, which is how the parity
    trailing a frame that completed from its data is seen. The first frame of a stream never
    ends this way (its parity may simply not have shown yet), and the first parity packet of a
    stream withdraws such a verdict from the newest frame;
  - any datagram, data or parity, of a later frame.
  A frame still pending `network.reassembly_reorder_grace_ms` after that instant is reported
  `FrameLost` by `poll_timeouts`, and `next_timeout` is the earlier of that deadline and the
  timeout, so the receive loops wake for it. Inside the grace a straggler or a parity packet
  completes the frame as always (`push` recovers before anything is declared lost); after it the
  frame's datagrams are stale like those of any lost frame. The recovery request for a frame
  whose final datagram arrived leaves one grace after it instead of when the next frame
  completes, about a frame interval earlier, and for the last frame before a static screen it
  leaves after the grace instead of the timeout. A frame whose tail was lost is noticed one
  grace after the next frame's first datagram instead of at its completion, which saves that
  frame's pacing span. The three older rules stay as the fallback: a lost tail with no frame
  after it still waits for the timeout. `0` turns the early verdict off. The rule holds for
  every `Reassembler`, the host's microphone one included: a one-fragment audio packet is
  complete on arrival, so it only matters for fragmented packets, and on the cursor it only
  brings forward a loss the next message would declare anyway.
- The grace is 2 ms by default. Nothing of a frame leaves after its final datagram and the next
  frame's first datagram leaves after it, so the grace never covers pacing, only a datagram that
  left before the trigger and arrives after it. The paced sender puts at least 250 µs
  (`MIN_BURST_GAP`) between bursts and does not wait after the last one, so only a datagram of
  the trigger's own burst, or one displaced by more than a burst gap, can be overtaken. A
  switched LAN keeps a flow in order (one path, FIFO queues, per-flow hashing on aggregated
  links) and Wi-Fi resequences in its block-ack window before IP sees the frames, which leaves
  the spreading of one flow over receive queues and cores, well under a millisecond. The receive
  loop takes one instant per `recvmmsg` batch and pushes everything already queued before it
  polls, so its own scheduling delay does not eat into the grace. 2 ms is eight burst gaps and
  several times that displacement, yet 12% of a 60 fps interval, 29% at 144 Hz and under half at
  240 Hz. A wrong verdict costs one recovery frame and never the stream, while a longer grace
  adds itself to the freeze after every real loss. The grace is not validated against the
  timeout, since whichever ends first loses the frame, so every existing file keeps loading.
  The wake is `SO_RCVTIMEO`, which the kernel rounds up to its tick, so the verdict can land a
  tick (1 ms at `HZ=1000`, 4 ms at 250) after the deadline. The default comes from this
  reasoning and from loopback tests; reordering on a real link was not measured.
- Frame numbers and sequences use serial-number arithmetic (`wrapping_sub` as `i32`), so wrap is
  transparent. A sender that restarts its frame numbering needs `Reassembler::reset`.
- `ReassemblyEvent::PacketsMissing` reports a sequence gap for `ControlMessage::ReportLoss`. A
  packet that arrives after its gap was reported was merely reordered, so loss reports are advisory.
