# Scaling
- [S] `scale-table-cache`: keep tap tables and their GPU buffers across frames instead of uploading them per pass
- [M] `scale-workgroup-tuning`: tune the workgroup shape and use shared-memory tiles for wide downscale kernels

# Video decode
- [S] `decode-rav1d-asm`: enable rav1d's `asm` feature (hand-written x86/ARM SIMD, needs `nasm` at build time) behind its own feature for faster software AV1 decode
