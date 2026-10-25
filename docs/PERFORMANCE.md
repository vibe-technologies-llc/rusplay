# Scaling
- [S] `scale-table-cache`: keep tap tables and their GPU buffers across frames instead of uploading them per pass
- [M] `scale-workgroup-tuning`: tune the workgroup shape and use shared-memory tiles for wide downscale kernels

# Networking
- [M] `net-batched-io`: `sendmmsg`/`recvmmsg` (or io_uring) batching for frame fragments
