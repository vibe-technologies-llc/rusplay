---
paths:
  - "crates/rusplay-input/**"
  - "crates/rusplay-client/src/input.rs"
---

# Input

- `InputEvent` is the wire-neutral model: keys by evdev keycode, pointer motion (relative) and
  position (absolute, normalised to the stream), buttons by evdev button code (`BTN_LEFT` is
  `0x110`), scroll in wheel notches (1.0 is one detent, positive is down and right), and
  gamepads by slot.
- `InputError` carries typed data and is not `Copy`. Backend failures are their own variants
  (`Portal { step, source }`, `EiIo`, `EiProtocol`, `Wayland*`, `Uinput*`, `PhysicalGamepad*`),
  never a message string.
- Platform backends sit behind cargo features of `rusplay-input` and are off by default, so the
  default build pulls in none of them. The host enables the ones it uses when it wires the sinks.
- The client captures input from its gpui window (`wayland` feature, below) and from physical
  gamepads (`gamepad` feature), with the pointer locked in relative mode or absolute inside the
  stream rectangle (`input.pointer_mode`).
- Input goes out the moment it is captured, never batched with media. The only merging is the
  `EventCoalescer` sum of motion deltas that piled up while the previous send was in progress.
- `wire` gives `InputEvent` a byte encoding for the protocol to carry: one tag byte, then
  little-endian fields, floats as raw IEEE bits, booleans as 0 or 1 (`encode_into` appends to a
  caller-owned buffer, `encoded_len` sizes it, `decode` takes exactly one event). Any other length,
  tag or boolean value is its own `Wire*` error. Gamepad events nest a second tag. The protocol
  crate wraps these bytes in a message and owns versioning; the layout changes only together with
  a protocol version bump.

## Host injection (`libei` feature)

- The host injects through the RemoteDesktop portal and libei, the Wayland way: no X11 XTest, and
  no uinput for keyboard and pointer. `ashpd` (`remote_desktop`, tokio) opens the session and
  `ConnectToEIS` hands over the socket; the pure-Rust `reis` crate speaks the EI protocol, so no
  libei C library is linked.
- `libei::mapping` is pure and unit-tested: `translate` turns an `InputEvent` into an `EiRequest`,
  and `AbsoluteMapper` turns normalised stream coordinates into the logical pixels of the regions
  the EIS server advertises. `LibeiInjector` only holds the connection state around them.
- `LibeiInjector::connect` performs the EI handshake with a blocking read (the portal socket
  answers at once), then everything is non-blocking: `inject` drains the socket, tracks seats and
  devices, binds the capabilities `input.keyboard` and `input.pointer` allow, and sends. It
  answers pings through the `reis` event converter, so it must be called regularly or the server
  may drop the client; the host calls it from its input loop.
- Devices start paused. `wait_ready` blocks until the compositor resumes one (the portal dialog
  may still be open). An event with no resumed device offering its capability returns
  `DeviceUnavailable`, and a device paused mid-session simply stops matching until resumed. A
  closed socket or `Disconnected` makes every later `inject` return `SinkClosed`.
- Each device is put in emulating state lazily (`start_emulating` with an increasing sequence)
  and every injected event is followed by its own `frame` with a `CLOCK_MONOTONIC` microsecond
  timestamp. Emulation is stopped when the injector drops.
- Keys are sent as evdev keycodes, which are layout independent, so the keymap the EIS server
  offers is never read. The host's own layout decides the symbols.
- Absolute motion maps onto the bounding box of the regions selected by `AbsoluteTarget`:
  `Bounds` takes every region of the device, `Mapping(id)` only those whose mapping id equals the
  id the ScreenCast stream reported (`PortalStream::mapping_id`). The upper edge maps just inside
  the region, because a point on the far edge lies outside it.
- Scroll goes out as `ei_scroll.scroll_discrete` in value120 units (`notches * 120`), so wheel
  notches reach games as real wheel steps while fractional notches stay high resolution.

### Sharing one portal session with ScreenCast

The RemoteDesktop session type also implements ashpd's `IsScreencastSession`, so the host can ask
for screen and input in a single portal session and a single permission dialog:

1. `RemoteDesktopSession::create()`.
2. Hand `session.session()` to `Screencast::select_sources` (and `open_pipe_wire_remote` later)
   in place of a ScreenCast-owned session.
3. `session.start(&PortalInputOptions::from_config(..))` selects the devices, starts the session
   and returns `StartedPortal`: the granted devices, the streams with node id, size and
   `mapping_id`, and the restore token covering both.
4. `session.connect_to_eis()` gives the fd for `LibeiInjector::connect`, then
   `injector.attach_session(session)` keeps the session alive for as long as the injector.

`LibeiInjector::open_standalone` runs steps 1, 3 and 4 for a host that wants input without a
ScreenCast session of its own. `rusplay-capture` currently opens its own ScreenCast session, so a
combined session needs `PortalSession` to accept an external one; that is part of `host-input`.

Live tests need a Wayland session with the portal and a person to confirm its dialog, so they are
`#[ignore]`d.

## Virtual gamepads (`gamepad` feature)

- Gamepads are virtual devices on the host so games see a native controller. They go through
  uinput with the pure-Rust `evdev` crate (`VirtualDevice`), not a portal: no portal exposes
  gamepad injection.
- Each connected slot is one uinput device imitating a wired Xbox 360 pad as the kernel `xpad`
  driver presents it: name `Microsoft X-Box 360 pad`, USB bus, vendor `045e`, product `028e`,
  version `0110`, so SDL, Steam and Proton apply their stock Xbox 360 mappings. Buttons are
  `BTN_SOUTH`, `BTN_EAST`, `BTN_NORTH` (the Xbox X), `BTN_WEST` (the Xbox Y), `BTN_TL`, `BTN_TR`,
  `BTN_SELECT`, `BTN_START`, `BTN_MODE`, `BTN_THUMBL`, `BTN_THUMBR`; axes are `ABS_X/Y/RX/RY`
  (-32768..32767, fuzz 16, flat 128), `ABS_Z/RZ` triggers (0..255) and `ABS_HAT0X/Y` (-1..1).
- `GamepadButton` and `GamepadAxis` carry fixed neutral ids, SDL's gamepad order, as associated
  constants (`GamepadButton::SOUTH`, `GamepadAxis::LEFT_X`, ...). The client maps its source
  (SDL, gilrs) onto them; unknown ids are `GamepadButtonUnknown` / `GamepadAxisUnknown`. Sticks
  are -1.0..1.0 with positive Y down, triggers 0.0..1.0, and values are clamped. The d-pad is four
  buttons that the mapper folds into the two hat axes, remembering which are held.
- `xbox360::Xbox360Mapper` (event to evdev `Report`) and `GamepadPool` (slot bookkeeping over a
  `GamepadFactory`) are pure and tested with fakes; only `uinput` touches the kernel.
  `GamepadPool::handle` enforces the slot limit (`GamepadSlotOutOfRange`, limit
  `input.max_gamepads`, default 4, valid 1..=16), creates the device on `Connected`, drops it on
  `Disconnected` (the kernel removes the pad) and rejects button and axis events for a slot that
  is not connected (`GamepadNotConnected`).
- `SplitSink` routes `InputEvent::Gamepad` to the gamepad sink and everything else to the
  desktop sink, so the host holds one `InputSink` made of `LibeiInjector` and `UinputGamepads`.
- Permissions: creating devices needs write access to `/dev/uinput`, and the `uinput` kernel
  module must be loaded (`modules-load.d` entry `uinput`). Do not run the host as root; grant the
  desktop user access with a udev rule such as
  `KERNEL=="uinput", SUBSYSTEM=="misc", OPTIONS+="static_node=uinput", TAG+="uaccess"`
  (logged-in seat users), or a group (`GROUP="input", MODE="0660"`) the user belongs to. Without
  it `Connected` fails with `UinputCreate` carrying the `PermissionDenied` source. The live tests
  are `#[ignore]`d and need that access.
- Each virtual pad advertises `FF_RUMBLE` only (like `xpad`) and `ff_effects_max` of
  `MAX_RUMBLE_EFFECTS` (16). The uinput fd is non-blocking. `GamepadPool::poll_rumble` (also
  `UinputGamepads::poll_rumble`) drains every connected pad's fd and appends `RumbleCommand`s; the
  host calls it from its input loop every few milliseconds, because the kernel blocks the game's
  `EVIOCSFF` upload until the owner answers it (30 s timeout, then the game sees an error).
- `RumbleTracker` (pure, one per pad, tested without hardware) holds the uploaded effects by the
  id the kernel assigned (`UI_FF_UPLOAD`), forgets them on `UI_FF_ERASE`, and turns `EV_FF` play
  events into commands: count `n > 0` starts the effect, `0` stops it. Only one effect is active
  per pad, the latest play wins, and stopping or erasing an effect that is not the active one
  emits nothing. Uploading over the active effect restarts it with the new strength. Unknown ids
  and non-rumble effect kinds are rejected to the game with `EINVAL`, or ignored for plays and
  erases, as the kernel would.
- `RumbleCommand { slot, strong, weak, delay_ms, duration_ms }` is what leaves `rusplay-input`:
  magnitudes are the kernel's `u16`, `duration_ms` is the effect length times the play count
  saturated at `u16::MAX`, and `0` means "until the next command for this slot" (the kernel's
  length `0`). A command with both magnitudes `0` is a stop (`RumbleCommand::stop`). FF gain and
  autocenter requests are not forwarded.

## Client capture

- `capture` is pure and always compiled. `CaptureCore` is the state machine every platform
  backend feeds with raw events (`key`, `pointer_button`, `pointer_motion`, `pointer_position`,
  `scroll`) and that appends `InputEvent`s to a caller-owned buffer. Nothing is forwarded until it
  is engaged: a button press while released engages (and the click is swallowed, its release too),
  and so does the release chord. `release` (chord, window focus lost, shutdown) first emits key and
  button releases for everything it had forwarded as held, so the host never keeps a stuck key.
  Keys that were already down when it engaged are swallowed until released.
- The release chord is `input.release_chord`, any left or right modifier of each member
  (`ctrl-alt-shift` by default), detected on the press that completes it, which is not forwarded.
  Pressing it again re-engages.
- Relative mode forwards `relative_pointer` deltas times `input.pointer_sensitivity` as
  `PointerMotion`. Absolute mode maps surface coordinates through the `StreamRegion` (the
  letterboxed picture rectangle in surface coordinates, set by the app) to `0.0..=1.0`, clamped,
  and drops positions while no valid region is set. `input.keyboard` and `input.pointer` gate each
  device.
- `EventCoalescer` merges consecutive `PointerMotion` and `Scroll` deltas, keeps only the latest
  `PointerAbsolute`, and keeps one value per (slot, axis) for pad axes, never across a key,
  button or pad connect or disconnect. Keys and buttons are never merged or reordered.
- `PadTranslator` turns raw evdev key and abs events of a physical pad into gamepad events: the
  inverse of `Xbox360Mapper` (`FaceLayout::Xpad`, evdev `BTN_NORTH` is the Xbox X) except for
  Sony pads (`FaceLayout::Positional`), sticks and triggers normalised through the device's own
  `RawAxis` range, `input.gamepad_deadzone` rescaled per stick axis, the hat folded into the four
  d-pad buttons, `BTN_TL2`/`BTN_TR2` turned into full trigger swings only on pads without
  `ABS_Z`/`ABS_RZ`, and unchanged axis values dropped. `PadSlots` hands out the lowest free slot
  below `input.max_gamepads` and keeps it for as long as the device stays.

### Physical gamepads (`gamepad` feature)

- `PhysicalGamepads` scans `/dev/input` for `event*` nodes once a second (opening only new
  ones, remembering non-gamepads, and dropping pads whose node vanished), accepts devices with
  `BTN_SOUTH`, `ABS_X` and `ABS_Y`, and `poll`s all pad fds together with `rustix::event::poll`,
  appending `Connected`, translated events and `Disconnected`. A read error removes the pad.
  `disconnect_all` removes every pad on shutdown. A host and a client on the same machine would
  capture the host's own virtual pads.
- Rumble: `PhysicalGamepads::rumble` uploads one `FF_RUMBLE` effect per pad on first use, updates
  it in place afterwards and plays it once. `duration_ms == 0` and anything above
  `input.rumble_max_ms` are capped to that limit, so an infinite effect whose stop was lost ends
  by itself. `input.rumble_max_ms = 0`, a slot without a pad or a pad without `FF_RUMBLE` make the
  command a no-op; a failed ioctl is `PhysicalGamepadRumble`. Dropping the pad erases the effect,
  which stops the motors.
- Reading `/dev/input/event*` needs the usual device ACL (`uaccess` on gamepads) or the `input`
  group; without it the pad is simply not found.
- Live tests (`#[ignore]`, need `/dev/uinput`) create a virtual pad, read it back through
  `PhysicalGamepads` and round-trip a rumble effect through the host side.

### Wayland capture (`wayland` feature)

- `wayland::WaylandCapture::attach(WaylandTarget { display, surface }, CaptureOptions, sink)` is
  `unsafe` because the pointers must stay valid: it wraps gpui's own `wl_display` with
  `Backend::from_foreign_display` (a second event queue on the same connection, so gpui keeps
  working), binds `wl_seat`, `zwp_pointer_constraints_v1` and `zwp_relative_pointer_manager_v1`
  (the last two required only in relative mode) and runs a thread that reads through
  `prepare_read` with a 10 ms poll, so it can be stopped. `WaylandTarget` takes the raw pointers
  so `rusplay-input` does not depend on gpui; the app gets them from gpui's raw window handles.
  The connection must outlive the capture: stop or drop it before the window.
- It makes its own `wl_pointer`, `wl_keyboard` and relative pointer on the shared seat. Keyboard
  keys arrive as raw evdev codes (layout independent, no xkb); pointer buttons likewise. Events
  count only while the pointer or keyboard focus is on the target surface; keyboard leave
  releases. Scroll uses `axis_value120` when the compositor sends it, else `axis` divided by 10
  per notch, flushed on `frame`.
- Engaging in relative mode requests `lock_pointer` with persistent lifetime (never twice: a
  second constraint on one pointer is a fatal protocol error for the whole connection), and
  releasing destroys it. `engage` and `release` can also be called from the app; they run on the
  capture thread within one poll interval. The local cursor is left alone: gpui owns the cursor
  surface, so hiding it is the app's job.
- Events go to the `sink` closure on the capture thread; the client passes
  `ClientStream::input()`'s `InputHandle`.
- Live tests (`#[ignore]`, need a compositor) attach to a plain `wl_surface` of a second
  connection on the same display.

## Wire contract between client and host

- Client input travels on `StreamKind::Input` datagrams (`rusplay_protocol::InputPacket`): the
  header `sequence` counts datagrams, the payload is a `current` section followed by a
  `previous` section, each a run of `[len u8][InputEvent::encode_into bytes]` records
  (`push_input_record`, `InputRecords`). `previous` repeats the preceding datagram's `current`.
- A host applies a datagram like this: `InputPacket::read`, then `InputSequence::admit(sequence)`:
  `Stale` is dropped, `Current` applies `current`, `PreviousAndCurrent` applies `previous` first
  and then `current` (one lost datagram is recovered, and the very first datagram recovers the
  lost sequence 0). Records decode with `InputEvent::decode`; a malformed one is dropped and
  counted. `InputSequence::reset` when a new client session starts. Events then go to the
  `SplitSink`, which routes gamepad events to `UinputGamepads` and the rest to `LibeiInjector`.
- The client sends one extra datagram with an empty `current` `input.resend_delay_ms` after any
  datagram that changed key, button or pad-button state (`0` disables), so a lost final release
  is recovered even when nothing follows. Two datagrams lost in a row still lose events; see
  `input-double-loss-stuck-key` in `docs/BUGS.md`.
- Gamepad `Connected` and `Disconnected` events create and drop the host's virtual pads, so the
  client only starts sending after the handshake.
- Rumble goes back on the control channel as `ControlMessage::Rumble { slot, strong, weak,
  delay_ms, duration_ms }`, a one-to-one copy of `RumbleCommand`, reliable and acknowledged so
  a stop is not lost. The host sends one per `poll_rumble` command. Messages are unordered, which
  is why the client caps every effect at `input.rumble_max_ms`.
- Host wiring (`host-input` in `docs/FEATURES.md`) therefore needs: a loop that reads
  `StreamKind::Input` datagrams into `InputSequence`/`InputEvent::decode`/`SplitSink::inject`,
  a call to `UinputGamepads::poll_rumble` every few milliseconds, and a
  `ControlMessage::Rumble` per command.
