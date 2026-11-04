# rusplay

Low-latency LAN remote play for Linux, written in Rust. Wayland only, PipeWire capture,
VAAPI/NVENC or fully configurable x264/x265/SVT-AV1 encoding with 4:2:2 and 4:4:4 chroma, GPU
scaling with proper filters, Opus or FLAC audio, and a custom UDP transport.

One machine hosts (captures, encodes and streams its screen and audio) and another connects
(receives, decodes, presents and forwards input). See [docs/ROADMAP.md](docs/ROADMAP.md) for
what is still to be built.

## Building

Both binaries are built with Cargo features that switch on the capture, codec and input backends;
a plain `cargo build --release` produces a host daemon that cannot capture (it stops at start with
`CaptureNotCompiled`) and an app without any video decoder.

Host daemon, `rusplay-hostd`:

```sh
cargo build --release -p rusplay-hostd --features vaapi,nvenc,software-encoders,pipewire,input,flac
```

Desktop app, `rusplay` (client, and host through the same encoder features as the daemon):

```sh
cargo build --release -p rusplay-app --features vaapi,nvdec,software,input
```

Drop the features of the backends you do not need. `vaapi,pipewire,input` is enough for a host
on the default configuration, whose encoder is VAAPI.

System packages needed at build time, by feature:

| Feature | Packages |
|---|---|
| `vaapi` | libva (with `libva-drm`) and libclang, which bindgen loads |
| `pipewire` | PipeWire development files |
| `software-encoders` | libx264, libx265, SVT-AV1 encoder (`libSvtAv1Enc`) and libclang, which bindgen loads |
| `software-asm` | `nasm` |

`nvenc` and `nvdec` link nothing at build time and load the NVIDIA driver libraries at run time.
Release builds target the host CPU (`target-cpu=native`). Build with
`--config 'target.x86_64-unknown-linux-gnu.rustflags=[]'` for a portable binary.

## Running

Start the host first, then connect from the client app.

- The host reads `$XDG_CONFIG_HOME/rusplay/host.toml` (`~/.config/rusplay/host.toml` when
  unset), or the file named by `rusplay-hostd --config <PATH>`; without either it runs on the
  defaults. The client app reads `$XDG_CONFIG_HOME/rusplay/client.toml` the same way. Both are
  optional and every setting has a default.
- On first start the host asks for screen sharing and input control through the desktop portal
  dialog.
- Pairing uses a PIN. The host prints a PIN to its terminal and issues a new one every pairing
  window. In the app pick the host from the Hosts page (hosts on the LAN are discovered
  automatically, or add an address) and enter the PIN. Both sides then remember each other, and
  later connects need no PIN.
- Paired clients are stored by the host in `$XDG_STATE_HOME/rusplay/paired-clients`
  (`~/.local/state/rusplay/paired-clients` when unset). List and remove them with
  `rusplay-hostd peers list` and `rusplay-hostd peers forget <ID|NAME>`; a forgotten client is
  refused at its next reconnect.
- Virtual gamepads on the host are created through `/dev/uinput`. The `uinput` kernel module must
  be loaded and the desktop user needs write access to the device, for example with the udev rule
  `KERNEL=="uinput", SUBSYSTEM=="misc", OPTIONS+="static_node=uinput", TAG+="uaccess"`. Without it
  gamepad connects fail; keyboard, pointer and touch use the portal and need no extra setup.
- With `input.gamepad_personality = "dualsense"` the host creates its virtual pads through
  `/dev/uhid` instead, as a USB DualSense that the kernel `hid-playstation` driver binds (module
  `uhid` loaded, `hid_playstation` available). `/dev/uhid` is root-only on a stock system, so the
  desktop user needs the udev rule `KERNEL=="uhid", SUBSYSTEM=="misc", OPTIONS+="static_node=uhid",
  TAG+="uaccess"`. Without it every gamepad connect fails and the host prints that access was
  denied once per session. Games then reach the pad like a real DualSense, so the usual Sony
  hidraw and gamepad rules apply to it.

## License

AGPL-3.0-only.
