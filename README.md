# rusplay

Low-latency LAN remote play for Linux, written in Rust. Wayland only, PipeWire capture,
VAAPI/NVENC or fully configurable x264/x265/SVT-AV1 encoding with 4:2:2 and 4:4:4 chroma, GPU
scaling with proper filters, Opus or FLAC audio, and a custom UDP transport.

Early scaffolding: see [docs/ROADMAP.md](docs/ROADMAP.md) for what is still to be built.

## Building

```sh
cargo build --release
```

Release builds target the host CPU (`target-cpu=native`). Build with
`--config 'target.x86_64-unknown-linux-gnu.rustflags=[]'` for a portable binary.

## License

AGPL-3.0-only.
