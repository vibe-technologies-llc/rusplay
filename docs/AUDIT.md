# Audit

Deep read of the workspace at `10fb801` (`master`, 2026-11-07). This file is the
findings. It does not change behaviour, and it does not repeat items already
listed in [BUGS.md](BUGS.md), [FEATURES.md](FEATURES.md) or
[PERFORMANCE.md](PERFORMANCE.md).

`cargo audit` (database 2026-11-06) reported no critical or high advisories and
no yanked crates in the workspace lockfile or in `fuzz/Cargo.lock`. Crate
dependency direction holds: media crates do not depend on `net`, and
`rusplay-encode` and `rusplay-decode` meet only as a dev-dependency.

## Scope

About 158k lines of Rust under `crates/`, plus the rules in `.claude/rules/`.
Passes covered correctness (decode, encode, host, client, net, audio),
security of the trust boundaries, the steady-state frame path, tests on the
critical paths, structure, dependencies, docs drift and direction.

Not audited, or only sampled:

- A live GPU session, a nested compositor, or a second machine. Hardware tests
  were not run for this report.
- `vendor/rav1d` beyond confirming that crates.io 1.1.0 still has the three
  local fixes (send-path unwrap, CDEF padding, picture-unref leak).
- The fuzz corpus and shader numerics. Colour and scale already have GPU tests
  described in the rules.
- gpui widget behaviour, and a line-coverage measurement of the suite.

## Findings

Ordered by leverage: impact against effort, discounted by confidence and by
how easily the fix itself goes wrong.

| # | Finding | Category | Impact | Effort | Risk | Confidence |
|---|---|---|---|---|---|---|
| 1 | HEVC VPS and SPS disagree on the long-term DPB | Correctness | Default long-term HEVC is non-conformant | S | MED | HIGH |
| 2 | Unsealed datagrams refresh the peer timer | Security | A spoofed source address holds the session open | S | LOW | HIGH |
| 3 | Disconnect can leave host input held | Correctness | Stuck keys until the peer timeout | S | LOW | HIGH |
| 4 | Session UDP sends are blocking | Correctness | Stop can hang in `join` | S | MED | HIGH |
| 5 | wgpu 30.0.1 aborts if a swapchain semaphore is still held | Dependencies | Resize or teardown can kill the process | M | MED | MED |
| 6 | Reassembly allocates and zeroes every video frame | Performance | Receive thread mallocs the frame before decode | S | MED | HIGH |
| 7 | Paced send allocates the `sendmmsg` vectors every burst | Performance | Send thread allocates on the default path | S | LOW | HIGH |
| 8 | The compose pass builds a bind group every draw | Performance | Window thread allocates a descriptor set per frame | S | LOW | HIGH |
| 9 | Control socket does not check the peer uid | Security | A connectable socket can mint a PIN | S | LOW | HIGH |
| 10 | Portal tokens and config are not written like peer secrets | Security | A loose directory or a planted staging file keeps its mode | M | LOW | HIGH |
| 11 | HEVC decode drops a reference the RPS kept | Correctness | A full 16-deep DPB loses an older picture | S | LOW | HIGH |
| 12 | Client-only settings are saved into `host.toml` | Debt | Host tab writes keys the host never reads | S | LOW | HIGH |
| 13 | Detached VAAPI decode panics on the first output picture | Correctness | The fuzz targets abort on a valid picture | S | LOW | HIGH |
| 14 | README app build does not match the default codec | Docs | The documented app command cannot software-decode HEVC | S | LOW | HIGH |
| 15 | Several recent fixes have tests that would still pass if the fix were reverted | Tests | Regression on the decode skip, handoff fallback, FFmpeg flush, and token split | M | LOW | HIGH |
| 16 | Takeover credits a frame the new client never received | Correctness | Host stats over-count that switch | S | LOW | HIGH |
| 17 | Audio overrun counters are samples, the rules say frames | Correctness | Surround loss is overstated in the panel | S | LOW | MED |
| 18 | 4:4:4 unpack still runs an identity chroma pass | Performance | An extra full-resolution dispatch on 4:4:4 | M | MED | HIGH |
| 19 | The comment hook treats `//` inside a multi-line raw string as a comment | DX | A valid edit is rejected | S | LOW | HIGH |

### 1. Make the HEVC VPS and SPS name the same decoded-picture buffer

- **Evidence**: `crates/rusplay-encode/src/vaapi/hevc.rs:290` — `vps()` always writes `ue(1)` for `vps_max_dec_pic_buffering_minus1` (two pictures). `hevc.rs:45` and `hevc.rs:331` — the SPS writes `3` when long-term references are on and `1` otherwise. `crates/rusplay-encode/src/vaapi/header_tests/hevc.rs:259` and `:371` lock that pair in (VPS 1, long-term SPS 3). `vps_base_layer_internal_flag` is set.
- **Evidence**: `.claude/rules/video-encode.md` describes one HEVC buffer of 4 and, earlier in the same file, three reconstructed surfaces. The code uses `long_term::RECONSTRUCTED_SURFACES` (4) for both H.264 and HEVC.
- **Impact**: H.265 requires those two buffering values to be equal when the base layer is internal. The long-term path, which is the one radeonsi gfx12 on Mesa 26.2+ actually sends, publishes a VPS buffer of 2 and an SPS buffer of 4. ffmpeg and `VaapiDecoder` accept the stream today (the hardware tests pass). A decoder that sizes the buffer from the VPS, or that rejects the mismatch, will not. The single-reference fallback is consistent (both values are 1).
- **Effort**: S.
- **Risk**: MED. The header tests assert the mismatch, and VPS and SPS have to change together. A wrong buffer size is the failure the long-term section already documents.
- **Confidence**: HIGH on the two writers. The equality rule is the H.265 constraint for `vps_base_layer_internal_flag`; it was not re-checked against a local copy of the spec.
- **Fix sketch**: Write one buffering value from both `vps()` and `sps()`, update the VPS header test, and make `video-encode.md` name both fields and `RECONSTRUCTED_SURFACES`.

### 2. Count only authenticated datagrams as peer liveness

- **Evidence**: `crates/rusplay-host/src/control.rs:183` — any datagram that the batch yields, whose UDP source equals the client, refreshes `last_heard` before it is routed. `crates/rusplay-client/src/receive.rs:360` does the same for the host address. `crates/rusplay-net/src/session.rs:22` — Discovery, Pairing and Reconnect are not sealed. `crates/rusplay-net/src/transport.rs:188` returns those unchanged once a session exists. `crates/rusplay-net/src/batch.rs:117` drops a failed open (length 0), so a bad AEAD tag does not refresh the timer.
- **Impact**: After the real peer is gone, a sender who can place a UDP datagram with that peer's source address keeps `ClientSilent` / `HostSilent` from firing. On the host that holds the only session: capture keeps running, held keys and pads are not released (`crates/rusplay-host/src/input.rs` releases them when the input stage exits), and discovery stays busy. An unprivileged `sendto` cannot forge another host's source address. This needs an on-path injector or a raw socket. The injector does not learn session keys.
- **Effort**: S.
- **Risk**: LOW. Sealed keepalives already flow while a real peer is up, including on a static screen.
- **Confidence**: HIGH.
- **Fix sketch**: Refresh `last_heard` only after a datagram whose stream kind requires sealing and whose open succeeded. Test that an unsealed header from the peer address does not postpone the timeout, and that a sealed keepalive still does.

### 3. Retransmit client Goodbye, and release input while the input stage is still alive

- **Evidence**: `crates/rusplay-client/src/receive.rs:951` — `say_goodbye` queues `Goodbye` once and flushes once. A full control window sends nothing, and there is no linger. `crates/rusplay-host/src/control.rs:678` — the host lingers for the acknowledgement.
- **Evidence**: `crates/rusplay-app/src/stream.rs:126` stops `ClientStream` (which joins every stage, including input) before `StreamWindow::stop`. `crates/rusplay-client/src/window.rs:411` releases held keys inside that later capture stop, into an `InputHandle` whose receiver is already gone. The host lifts keys in `release_all` when its input stage exits, which a client `Goodbye` triggers; otherwise it waits for `network.peer_timeout_ms` (default 5000).
- **Impact**: A disconnect whose single Goodbye is lost, or never queued, leaves the host holding keys, buttons and virtual pads until the peer timeout. The capture's own release events cannot repair that.
- **Effort**: S.
- **Risk**: LOW. The host already lingers. Disconnect becomes slightly slower when the acknowledgement is late.
- **Confidence**: HIGH.
- **Fix sketch**: On stop, retransmit Goodbye until it is acknowledged or a short linger expires, including after `ControlWindowFull`. Release capture input before joining the client input stage.

### 4. Make session sends non-blocking and stop-aware

- **Evidence**: `crates/rusplay-net/src/transport.rs:214` — `set_nonblocking` exists, and the session bind does not call it. The only session-socket callers that do are tests. `crates/rusplay-net/src/batch.rs:190` — `sendmmsg` therefore waits in the kernel when the send buffer is full. `crates/rusplay-host/src/send.rs:230` — `send_all` only `yield_now`s on a zero count and returns `Ok` when the suspension count changes, but a blocking send never reaches that check. `HostStream::stop` joins that thread after setting the stop flag.
- **Impact**: Stopping a host or client can sit in `join` for as long as a send is stuck (full send buffer, wedged interface). The networking rule that a short `sendmmsg` is re-queued assumes a non-blocking socket.
- **Effort**: S.
- **Risk**: MED. The full-buffer path must back off instead of spinning, and pacing must still sleep on its own timer rather than inside the syscall.
- **Confidence**: HIGH.
- **Fix sketch**: Set the session socket non-blocking after bind. In `send_all`, on `WouldBlock` or a short count, wait with a timeout and return if stop is set.

### 5. Move the stream wgpu off crates.io 30.0.1

- **Evidence**: Root `Cargo.toml` pins workspace `wgpu` to crates.io `30.0.1`. The lockfile also has gpui's git `wgpu` 30.0.0. They are different crates; no handle is shared. Published `wgpu-hal` 30.0.1 `Swapchain::release_resources` `expect`s a unique acquire semaphore and a unique present semaphore (`wgpu-hal-30.0.1/src/vulkan/swapchain/native.rs:400` and `:409`) after `device_wait_idle`. The rev gpui already uses replaces that `expect` with a skip. Release builds set `panic = "abort"`. `crates/rusplay-client/src/window.rs` reconfigures the surface on a size change and drops it when the window thread joins.
- **Impact**: The video surface is the crates.io copy. A resize or disconnect that releases the swapchain while a `SurfaceTexture` still holds a semaphore aborts the process instead of returning `SurfaceLost`. Idle is attempted first, so this is the case where the `Arc` is still shared after the wait. Bumping the gpui rev does not fix presentation.
- **Effort**: M.
- **Risk**: MED. A git wgpu is not a strict superset of 30.0.1. `ash` 0.38 has to stay the version wgpu-hal uses, because DMA-BUF handles cross that boundary.
- **Confidence**: MED. The `expect` and the call sites are certain. Whether a rusplay resize holds the semaphore across `configure` was not reproduced here.
- **Fix sketch**: Point the workspace `wgpu` at a gfx-rs rev that contains the semaphore fix and still uses `ash` 0.38. Keep the two wgpu crate identities. Re-test surface configure, present failure and window drop.

### 6. Recycle reassembled video frames and stop zeroing the whole buffer

- **Evidence**: `crates/rusplay-net/src/reassembly.rs:121` — each frame starts from `PendingFrame::new` unless one is already pending. `reassembly.rs:416` — the first fragment allocates `vec![0; count * shard_len]`. `reassembly.rs:628` — `assemble` moves that buffer out, so the next frame allocates again. `reassembly.rs:519` — every parity shard is `parity.to_vec()`, including when the frame arrives intact. Video FEC defaults to 10%.
- **Impact**: The receive thread mallocs and memsets the padded frame, then copies the fragments over those zeros, before decode starts. Only the unread tail of the last shard has to be zero for FEC. The no-loss path also allocates about a tenth of the frame as one `Vec` per parity shard.
- **Effort**: S.
- **Risk**: MED. A recycled buffer must still zero the last shard's padding, or a recovery checksums wrong.
- **Confidence**: HIGH.
- **Fix sketch**: Keep one frame buffer and one parity slab on `Reassembler`. Zero only the unread tail. Copy parity into the slab.

### 7. Stop allocating the sendmmsg vectors on every burst

- **Evidence**: `crates/rusplay-net/src/batch.rs:169` — `send_arena` collects a new `Vec<[IoSlice; 2]>` for the range. `batch.rs:180` — `send_wire` then allocates the ancillary and `MMsgHdr` vectors for that chunk. `crates/rusplay-host/src/send.rs:205` — pacing, which is the default, calls this once per burst. `MIN_BURST_GAP` is 250 µs, so a large keyframe is many bursts.
- **Impact**: The datagram arena is reused. The syscall vectors are not. The default paced send heap-allocates three vectors per burst on every video frame, including the DMA-BUF path.
- **Effort**: S.
- **Risk**: LOW. The buffers live across one `sendmmsg`.
- **Confidence**: HIGH.
- **Fix sketch**: Store the iovec, ancillary and header vectors on `Transport` and refill them.

### 8. Cache the window compose bind group

- **Evidence**: `crates/rusplay-client/src/window/compose.rs:138` — `Composer::encode` calls `device.create_bind_group` on every draw. The swapchain image is the render attachment, not a binding. The scaler and the unpacker already keep a `GroupCache`.
- **Impact**: Every displayed frame allocates a bind group and a Vulkan descriptor set on the window thread, immediately before submit and present, even when the picture is one of the pooled textures and the cursor and overlay views have not changed.
- **Effort**: S.
- **Risk**: LOW. Invalidate the cache when the cursor or overlay view changes.
- **Confidence**: HIGH.
- **Fix sketch**: Key a small group cache by the picture, cursor and overlay views. Write the params uniform only when placement or encoding changes.

### 9. Require the owner's uid on the control socket before issuing a PIN

- **Evidence**: `crates/rusplay-hostd/src/control_server.rs:163` — the socket is bound, then its mode is set to `0600`. Nothing reads `SO_PEERCRED`. `control_server.rs:104` — `new-pin` generates a PIN, installs it as the next pairing PIN, and returns it on the socket. `forget` revokes a paired client in the live store. `peers.control_socket_path` is used as given. The default path is `$XDG_RUNTIME_DIR/rusplay/control.sock`.
- **Impact**: Anyone who can connect learns a live PIN and can then pair over the network, or can revoke pairings. `XDG_RUNTIME_DIR` is specified to be `0700`, so other users cannot name the default socket. The code never checks that. A configured path in a traversable directory, or a connection accepted in the window after bind and before `chmod`, is enough. Bind creates the socket at `0777` masked by the umask; `0700` requested later is not the mode of that first instant.
- **Effort**: S.
- **Risk**: LOW. The daemon and the CLI run as the same user.
- **Confidence**: HIGH.
- **Fix sketch**: `fchmod` the listening fd before it can accept, and drop the connection unless the peer uid is the effective uid. Refuse to listen when the parent directory is not owner-only.

### 10. Write portal tokens and config with the same owner-only helper as peer secrets

- **Evidence**: `crates/rusplay-net/src/peers.rs:383` — `write_owner_only` creates the parent at `0700`, opens with `create_new` at `0600`, syncs, and renames. A test plants a `0666` staging file and requires the replaced store to stay `0600`.
- **Evidence**: `crates/rusplay-capture/src/token.rs:65` — the restore-token writer uses `create_dir_all` (umask, not `0700`) and `create` + `truncate`. On Linux the mode applies only when the inode is created, so an existing staging file keeps its mode across the rename. The test only checks a fresh file.
- **Evidence**: `crates/rusplay-config/src/lib.rs:108` — `Config::save` uses `fs::write` on a sibling temporary, which follows a final symlink and creates the file as `0666` masked by the umask. `crates/rusplay-hostd/src/pin_file.rs:98` — the PIN writer uses `create_new` and `0700`, but `remove_file` errors are discarded, there is no `sync_all`, and `read` uses `read_to_string`, which follows a symlink.
- **Impact**: The restore token is what lets the next start skip the screen-cast dialog. The default state directory is tightened to `0700` when the peer store opens, and a mode of `0700` is not loosened by the umask, so another user cannot plant a staging file there after that. A configured `capture.restore_token_file` or `peers.pin_file` in a shared directory does not get that guarantee. `rusplay-hostd pin` would print whatever a replaced symlink names, including the peer store, if the pin path's directory is writable by someone else. World-readable `client.toml` / `host.toml` is the same pattern at lower sensitivity. The peer-secret writer already refuses it.
- **Effort**: M.
- **Risk**: LOW. The bytes and the paths stay. The token staging name changes from `with_extension` to `with_added_extension` only when the configured path already has an extension.
- **Confidence**: HIGH on the writers. The cross-user case is the configured path, not the default runtime directory.
- **Fix sketch**: Route the token store, the PIN file and `Config::save` through `write_owner_only`. On read, reject a non-regular file. Move the planted-staging test next to the helper.

### 11. Stop evicting an HEVC reference the RPS still needs

- **Evidence**: `crates/rusplay-decode/src/vaapi/hevc.rs:498` — `apply_reference_set` already keeps only the pictures the current RPS names. `hevc.rs:25` and `hevc.rs:319` — if that set still has 15 entries, the decoder `remove(0)`s the oldest and only afterwards pushes the picture just decoded. The VA `ReferenceFrames` array is built before that push, so it has at most 15 slots.
- **Impact**: A full decoded-picture buffer that is not a pure sliding window loses the oldest picture after a successful decode. The next picture comes back `ReferenceMissing`, or, if another buffered picture shares that POC LSB, predicts from the wrong surface and is reported good. Rusplay's own VAAPI encoder stays at a DPB of 4, so this does not trip on that stream. It trips on other HEVC that fills a 16-deep buffer.
- **Effort**: S.
- **Risk**: LOW. Trust the RPS up to 16 stored pictures. The reference array is still built before the current picture is pushed.
- **Confidence**: HIGH.
- **Fix sketch**: Drop the post-RPS `remove(0)`. Size the stored vec for `sps_max_dec_pic_buffering`. Add a regression that keeps a long-term picture at index 0 while fifteen short-term pictures rotate.

### 12. Classify settings role on the field, not with an unanchored prefix

- **Evidence**: `crates/rusplay-app/src/settings/schema.rs:325` — `audience` is a separate prefix and path list. Anything left over is `Shared`. `"video.encoder"` also matches `video.encoder_gpu_handoff` (host-only today, so the accident is harmless). `"input.hotkey"` also matches `input.hotkeys`. `schema.rs:1417` — `input.escape_hold_ms` and `input.fullscreen_hint_ms` are in neither list. Both are client capture and fullscreen-hint behaviour. The host never reads them. `every_serialized_option_has_a_form_field` checks that a key has a row. It does not check who sees the row.
- **Impact**: Those two settings appear on the Host tab and are written into `host.toml`, where they do nothing. They also appear on the Client tab, so the client file is fine. A new role-specific key has the same default: the row test stays green and the wrong role file keeps the value.
- **Effort**: S.
- **Risk**: LOW. Moving the two keys to `Client` stops writing them into `host.toml`. Keys already stored there are still valid config fields; they simply stop being rewritten.
- **Confidence**: HIGH.
- **Fix sketch**: Put `Audience` on `Field` and delete the prefix lists. Test that every row's audience is the one the form uses. Anchor any remaining prefix at a dot.

`HostProfile::overlay` (`crates/rusplay-config/src/profile.rs:29`) is a third hand-written list of the same shape. Adding a field fails `full_profile()` at compile time, but a field that is added to the fixture and not to `overlay` is stored in the host book and never applied. One exhaustive test that every set field changes the merged `Config` closes it. The same client receive match ends in `ControlEvent::Received(_) => {}` (`crates/rusplay-client/src/receive.rs:697`). Every host-to-client message that exists today is named above that wildcard. The next one is acknowledged and dropped, and the host side would not compile if it forgot a variant.

### 13. Do not abort detached VAAPI decode on the first output picture

- **Evidence**: `crates/rusplay-decode/src/vaapi/va.rs:182` — with the `fuzzing` feature, `acquire` pushes a `PoolSurface::detached` and never pushes a `Surface` into `owned`. `va.rs:155` — `export_output` looks that id up in `owned` and `expect`s it. Every outputted picture calls `export_output`. The `decode_h264`, `decode_hevc` and `decode_av1` fuzz targets decode only through `VaapiDecoder::detached`. `--all-features` turns `fuzzing` on. Release builds use `panic = "abort"`.
- **Impact**: The first picture with `pic_output_flag` set aborts the process. The fuzz targets never exercise a successful decode. A corpus that contains a valid picture is reported as a crash. Parser failures that happen before output still return errors. Production acquire still pushes `owned`.
- **Effort**: S.
- **Risk**: LOW.
- **Confidence**: HIGH.
- **Fix sketch**: On a detached display, return the empty image the detached surface already carries, or return a `DecodeError` instead of panicking. Decode one fixture access unit through `VaapiDecoder::detached` and require `Ok(Some(_))`.

### 14. Fix the README app build so it matches the default codec

- **Evidence**: `README.md:23` says the desktop app hosts through the same encoder features as the daemon. The daemon command on line 20 is `vaapi,nvenc,software-encoders,pipewire,input,flac`. The app command on line 26 is `vaapi,nvdec,software,input`. `software` is the rav1d AV1 decoder. H.264 and HEVC software decode are the `ffmpeg` feature. The default codec is HEVC. `CLAUDE.md` includes `ffmpeg` in the app command. The package table has no libavcodec row. `pipewire` and `flac` are default features of `rusplay-app`, so those two are not missing.
- **Impact**: Copying the README command cannot host with NVENC or the software encoders, despite the sentence above it, and cannot software-decode the default HEVC stream. A machine without a working VAAPI or NVDEC decoder fails at connect with no documented feature to turn on.
- **Effort**: S.
- **Risk**: LOW. Documentation only. The `ffmpeg` feature needs libavcodec, libavutil and libclang at build time.
- **Confidence**: HIGH.
- **Fix sketch**: Show the app command with `ffmpeg` and the host encoder features the sentence promises, and add those packages to the table. Say that `software` alone is AV1.

### 15. Lock the fixes the current tests would not notice being reverted

These are test gaps, not open behaviour bugs. The code under them was read and does the thing the name claims.

- **Decode skip.** `crates/rusplay-client/src/decode.rs:111` drops non-keyframes while `skipping` is set, and a decode error sets that flag. `crates/rusplay-client/src/receive.rs` tests the receive side. Nothing drives the decode loop with a failing inter frame followed by another inter frame and then a keyframe. Deleting the skip presents the queued delta and the receive test still passes. `a_decoder_open_request_opens_the_decoder_before_any_frame` accepts `Ok(())` and `frames_decoded == 0` under `ffmpeg`, which is also what an ignored `Open` produces once the channel closes.
- **GPU handoff fallback.** `crates/rusplay-host/src/pipeline.rs:633` turns handoff off and re-encodes the same frame from the readback. `crates/rusplay-host/src/pipeline/tests/handoff.rs:162` points VAAPI at `/dev/null` and expects `Err` plus `hands_off() == false`. The readback encoder fails the same way, so a picture is never required. `handoff_disabled` is never read. A conversion failure while `Trying` (`pipeline.rs:545`) also forces `Off` and does not increment the counter, and has no test.
- **FFmpeg reference loss.** `crates/rusplay-decode/src/ffmpeg/mod.rs:131` returns the send error and does not call `avcodec_flush_buffers`. `crates/rusplay-decode/tests/ffmpeg.rs:249` skips a packet, expects `DecodeError::Ffmpeg`, then submits the first packet again and only checks `is_some()`. An IDR still decodes if the flush is put back.
- **Capture and input token split.** `RememberedDisplay::forget` (`crates/rusplay-host/src/display.rs:13`) clears the capture store only. The input token is the sibling `input-restore-token`. No test writes that sibling and asserts it survives Choose display.

- **Effort**: S for each assertion, M for a handoff test that fails the import and still encodes.
- **Risk**: LOW, except the handoff test, which must reject DMA-BUF without rejecting the shared-memory encoder.
- **Confidence**: HIGH.
- **Fix sketch**: Assert the property the fix exists for: frames skipped inside the decode loop, `Ok` and `handoff_disabled == 1` after a refused import, a dependent frame that still names the kept picture after the FFmpeg error, and the input token file left in place.

Two smaller test notes. `crates/rusplay-client/tests/loopback.rs` sleeps through `display_debounce_ms` for a property `resize.rs` already locks with explicit instants; a scheduler stall longer than the debounce fails the loopback test on a loaded machine. Esc-hold is covered in `CaptureCore`. The production call is `Session::tick`, and `ExitFullscreen` is applied only when the window is already fullscreen. Neither caller has a test.

### 16. Do not count a send aborted by takeover as sent

- **Evidence**: `crates/rusplay-host/src/send.rs:131` — a frame whose suspension count is already stale on dequeue is `frames_dropped`. `send.rs:230` — `send_all` returns `Ok(())` as soon as the suspension count changes, including after sending nothing or only a prefix. `send.rs:184` — `send_frame` then always increments `frames_sent`, adds `arena.len()` to `datagrams_sent`, and counts a keyframe.
- **Impact**: A takeover that lands during pacing credits a full frame and every datagram, including a keyframe the new client never got. The interval and final host stats over-count that switch. Encode already forces the next keyframe from `Mailbox::suspend`, so the picture path is unaffected.
- **Effort**: S.
- **Risk**: LOW.
- **Confidence**: HIGH.
- **Fix sketch**: Report how many datagrams were handed to the socket. Increment `frames_sent` and `keyframes_sent` only when the whole frame went out.

### 17. Publish audio overruns and overflows in frames

- **Evidence**: `crates/rusplay-audio/src/ring.rs:29` — `RingWriter::write` returns `samples.len() - writable`. `capture.rs:186` adds that value to `overruns`. Playback adds the same kind of count to `overflows`, which the client copies to `audio_overflows` and the host to `microphone_overflows`. The capture unit test expects `7` for seven extra samples. The audio rules count dropped frames.
- **Impact**: On stereo the published counters are twice the frame count. On 5.1 and 7.1 they are six and eight times too high, so the stream panel overstates loss. The ring still refuses to split a frame.
- **Effort**: S.
- **Risk**: LOW. The tests that assert sample counts have to move with the unit.
- **Confidence**: MED. The code and the tests agree on samples. The rules say frames. Confirm the unit before switching the stat.
- **Fix sketch**: Divide by the channel count at the capture and playback counters.

### 18. Skip the chroma dispatch when 4:4:4 is already full resolution

- **Evidence**: `crates/rusplay-scale/src/unpack.rs:612` — `UnpackPlan::encode` always dispatches `rusplay-unpack-chroma` and then `rusplay-unpack-rgb`. The rules state that 4:4:4 horizontal and vertical taps are `TapTable::identity`. The chroma pass writes a full-size `rg32float` intermediate.
- **Impact**: A 4:4:4 frame pays a full-resolution compute pass that copies CbCr through a one-tap kernel, then the RGB pass reads that copy back. 4:2:0 and 4:2:2 still need the horizontal pass. The default stream chroma is 4:2:2, so this is the 4:4:4 client path (software and NVDEC).
- **Effort**: M.
- **Risk**: MED. Left versus centre siting must stay a no-op at 1:1, and the fused chain and the LUT both use this plan.
- **Confidence**: HIGH that the dispatch is unconditional. The identity tables are what the scaling rules specify for 4:4:4.
- **Fix sketch**: When both tap tables are identity, sample chroma in the RGB shader and do not dispatch the chroma pass.

### 19. Teach the comment hook to ignore `//` inside multi-line raw strings

- **Evidence**: `.claude/hooks/rust-edit.sh` runs `scripts/check-comments.pl` on every edited `.rs`, `.wgsl` and `.toml` file and exits 2. `scripts/check-comments.pl:7` strips a raw string only on the current line. The TOML scanner in the same file keeps state across lines. Config tests are multi-line raw strings.
- **Impact**: A later line of a raw string that contains `//` (a URL, a TOML fixture, a WGSL snippet) is reported as a banned comment, and the hook tells the editor to delete it.
- **Effort**: S.
- **Risk**: LOW. A wrong state machine could start allowing real comments. Keep the exit for comments outside strings.
- **Confidence**: HIGH.
- **Fix sketch**: Track raw-string state across lines the way the TOML scanner already does, and add a fixture line with `http://` inside an open raw string that must not fail.

## Direction

These are options, not defects. The roadmap already lists the unfinished hardware ports, virtual-output live checks, pacing, clipboard live checks and long-term reference work. They are not repeated here.

**Discovery past the default IPv4 route.** `networking.md` states that discovery is IPv4 only, that limited broadcast leaves through the default route, and that the multicast TTL is 1, so another interface or VLAN is invisible. `discovery_group` is an `Ipv4Addr`, and `docs/FEATURES.md` has no item for this. Typing an address still works. A design spike for one extra query path (IPv6 link-local, or IPv4 multicast on each up interface) is the proportional next step. Do not change pairing. Effort is coarse and large. Confidence that the limit is real and unscheduled is high.

**Host JSON stats from the in-app host, or stop offering the flag there.** `network.stats_json` is a shared settings row. The app writes client lines only, and `ui.md` says that is the only stdout besides `--version`. `rusplay-hostd` writes the same shape for `HostStats`. Someone hosting from the app cannot diff host JSON the way the daemon plus the app can. Saving the flag in `host.toml` does nothing until the daemon is used. Either the host page prints `HostStats` with a distinct `event`, or the flag is daemon-only in the form. Two writers on one stdout will interleave if the app is also the client. Effort is small to medium. Confidence is high.

## Considered and rejected

- **Unknown control tags decode as `Goodbye`.** They do not. The length table returns `UnknownControlTag`. The trailing `_ => Goodbye` is `TAG_GOODBYE`, which is in that table and not named again. `every_message()` is a fixed array of 16 and already omits `Display`, which has its own test. Worth closing when the next message is added, not on its own.
- **Move ashpd to 0.14 / zbus 6.** `cargo audit` reports the zvariant 5 line unmaintained, with no critical or high advisory. ashpd 0.14 depends on zbus 6. gpui still declares ashpd 0.13. Moving one of them puts two D-Bus stacks in the binary and breaks the one-stack rule. Not worth doing until the gpui rev moves.
- **Take spake2 0.5 to drop the second `sha2` / `hkdf` stack.** The only newer release is `0.5.0-pre.0`. The pairing transcript would change under a crate that is not a stable release. No advisory on either stack.
- **A general codec dump, or a third headless client binary.** The HEVC dump exists for one open import bug. A supported client binary would drift from the app on present, input and clipboard. The library already runs the loopback tests.
- **`peers.host_name` allows a control character that discovery rejects.** `DiscoveryResponder::bind` fails closed through `to_datagram`, so the host does not start. `client_name` already rejects control characters. A field error would be clearer. The host does not announce a bad name.
- **rav1d `EAGAIN` on both send and get.** `software.rs:125` breaks and returns `Ok(None)` if `dav1d_get_picture` also returns `WouldBlock`, and the input is dropped. `max_frame_delay` is 1. Whether that pair happens was not shown. Investigate before changing it: if it does, the access unit is discarded and `decode_errors` stays zero.
- **Per-frame `vaCreateBuffer` / `vaDestroyBuffer`, and the DMA-BUF import cache's FIFO eviction.** Both are plausible steady-state costs. Some drivers treat a VA buffer as single-use until it is destroyed, and the AV1 surface-pool high-water mark was not measured against the 16-entry cache. Not worth a change until a profile says they are on the critical path.
- **Missing CI.** Not a defect for this repository.
