---
paths:
  - "crates/rusplay-input/**"
---

# Input

- `InputEvent` is the wire-neutral model: keys by evdev keycode, pointer motion (relative) and
  position (absolute, normalised to the stream), buttons by evdev button code (`BTN_LEFT` is
  `0x110`), scroll in wheel notches (1.0 is one detent, positive is down and right), and
  gamepads by slot.
- `InputError` carries typed data and is not `Copy`. Backend failures are their own variants
  (`Portal { step, source }`, `EiIo`, `EiProtocol`), never a message string.
- Platform backends sit behind cargo features of `rusplay-input` and are off by default, so the
  default build pulls in none of them. The host enables the ones it uses when it wires the sinks.
- The client captures input from its gpui window, with the pointer locked in relative mode
  (`input.pointer_mode`).
- Input goes out the moment it is captured, never batched with media.
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
- Rumble and force feedback are not implemented; see `input-gamepad-rumble` in
  `docs/FEATURES.md`.
