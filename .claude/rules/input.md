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
combined session needs `PortalSession` to accept an external one; that is part of wiring the host
pipeline.

Live tests need a Wayland session with the portal and a person to confirm its dialog, so they are
`#[ignore]`d.
