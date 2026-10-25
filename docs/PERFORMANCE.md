# Scaling
- [M] `scale-convert-single-encode`: apply the transfer function once per source pixel in `ColorConverter` (luma and chroma passes each evaluate it today) and write 4:4:4 chroma without the `rg32float` intermediate; 4:4:4 10-bit at 4K takes 0.66 ms against 0.37 ms for 4:2:0

# Networking
- [M] `net-batched-io`: `sendmmsg`/`recvmmsg` (or io_uring) batching for frame fragments
- [S] `net-socket-buffers`: apply `network.socket_buffer_bytes` to the socket send/receive buffers
