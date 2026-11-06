These HEVC fixtures exercise Rusplay's production VAAPI encoder with moving
1280x720 `testsrc2` frames at 60 fps, constant-quality QP 20, and long-term
references. They were generated on Mesa 26.2.6, radeonsi gfx1201. Motion fixtures
contain 64 frames at 8-bit and 10-bit. The 10-bit recovery fixture drops encoded
frames 21 through 31, requests recovery from frame 20, and resumes at frame 32,
leaving 53 delivered frames.

Each `.index` line contains the encoded packet length followed by three
hexadecimal FNV-1a 64-bit checksums for the Y, U and V planes. Reference pixels
come from FFmpeg software HEVC decoding, in planar YUV 4:2:0. Ten-bit samples
are little-endian with samples in the low ten bits. Checksums exclude row
padding. Packet lengths preserve access-unit boundaries without requiring AUDs.

Run the portable comparison on the affected client:

```sh
RUSPLAY_REQUIRE_HW=1 cargo test -p rusplay-decode --features vaapi --test vaapi_motion -- --nocapture
```

This compares Rusplay VAAPI CPU-copy output and FFmpeg VAAPI output separately
against the software reference, without networking, Vulkan import, scaling or
presentation. FFmpeg's comparison visibly skips if its executable or VAAPI
backend is missing. A mismatch identifies the sample, delivered frame and plane.

When CPU copy output is clean and DMA-BUF output is not, run the pipelined
import comparison on that client:

```sh
RUSPLAY_REQUIRE_HW=1 cargo test -p rusplay-decode --features vaapi --test vaapi_pipeline -- --ignored --nocapture
```

It decodes on one thread and reads through Vulkan on another, with frames in
flight as in the client, and prints the VAAPI device, the Vulkan adapter and the
surface layout. `RUSPLAY_VAAPI_DEVICE` selects a render node other than the
default. Each sample runs three ways: `Direct` is the client's path, `Fenced`
waits for the dma-buf write fence before reading, and `Held` keeps each surface
out of reuse until its readback is checked. A device and adapter on different
GPUs, or a mode that alone comes back clean, names the cause.

Regenerate from the repository root on hardware whose long-term references are
verified, with FFmpeg installed:

```sh
RUSPLAY_REQUIRE_HW=1 RUSPLAY_HEVC_FIXTURE_DIR="$PWD/crates/rusplay-decode/tests/data" cargo test -p rusplay-encode --all-features hevc_luma_and_chroma_match_software_decoding -- --nocapture
```

Regeneration changes both bitstreams and reference checksums. The test compares
all decoded bytes with the software reference before succeeding.
