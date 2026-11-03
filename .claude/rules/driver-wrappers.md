---
paths:
  - "crates/rusplay-va/**"
  - "crates/rusplay-cuda/**"
---

# Driver wrapper crates

- `rusplay-va` and `rusplay-cuda` sit directly above `rusplay-core` (`rusplay-cuda` needs nothing
  but `libloading`) and below the media crates. Each holds the primitives the encode and decode
  backends of one driver would otherwise copy: the `unsafe` calls, their `Drop`s, the status check
  and the error enum. A sideways edge between `rusplay-encode` and `rusplay-decode` stays
  forbidden; a crate below both is the way to share. Codec-specific code (parameter buffers,
  packed headers, the coded buffer, DPBs, cuvid and NVENC function tables, DMA-BUF image types)
  stays in encode and decode, built on the wrappers' public API.
- `rusplay-va` is empty unless its `libva` feature is on, which `rusplay-encode/vaapi` and
  `rusplay-decode/vaapi` enable together with the optional dependency. `build.rs` generates the
  libva bindings with bindgen from the headers `pkg-config` finds for `libva` and `libva-drm`
  (macro constants unsigned) into `OUT_DIR/va.rs`, and `ffi` is the public module that includes
  them; the backends re-export it as their local `ffi`. The crate owns `Display` (open, profile,
  entry point and attribute queries, sync), `Config`, `Context` (parameter, data and empty
  buffers, `render`), `Buffer` (id, map, unmap), `Surface` (create with a usage hint and pixel
  format, import from DMA-BUF objects and planes, export read-only or read-write), `Image` (create,
  write, put, get) and `SizeLimits`. Objects share an `Rc<Display>`, so none is `Send`; the
  backends own the whole set behind one `unsafe impl Send`. `VaError` is `DeviceOpen`,
  `DisplayUnavailable` or `Call { call: VaCall, status: VaStatus }`.
- `rusplay-cuda` always builds and links nothing: `Driver::load_from(name)` opens `libcuda.so.1`
  with `libloading`, resolves `cuInit`, `cuDeviceGetCount`, `cuDeviceGet` and the v2 context push
  and pop, and offers `symbol` for the extras a backend needs; `device(index)` runs `cuInit`, the
  count check and `cuDeviceGet`; `push_current` returns a `Current` guard that pops on drop.
  `open_library`, `symbol` and `check` are public because NVENC's and NVDEC's own libraries load
  the same way. `CudaError` is `LibraryNotFound`, `SymbolMissing`, `DeviceMissing` or
  `Call { call: CudaCall, status: CudaStatus }`; `CudaCall` names the driver-API calls and the
  cuvid calls are NVDEC's own `CuvidCall`. `ffi` holds the shared `CuResult`, `CuDevice`,
  `CuContext` and function pointer types.
- Both crates' errors are wrapped transparently with `#[from]` in `EncodeError` and `DecodeError`.
