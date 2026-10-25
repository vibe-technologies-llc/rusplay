# Scaling
- [M] `scale-convert-single-encode`: apply the transfer function once per source pixel in `ColorConverter` (luma and chroma passes each evaluate it today) and write 4:4:4 chroma without the `rg32float` intermediate; 4:4:4 10-bit at 4K takes 0.66 ms against 0.37 ms for 4:2:0

# Video decode
- [S] `decode-rav1d-asm`: enable rav1d's `asm` feature (hand-written x86/ARM SIMD, needs `nasm` at build time) behind its own feature for faster software AV1 decode
