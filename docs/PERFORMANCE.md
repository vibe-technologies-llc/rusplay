# Scaling
- [M] `scale-convert-single-encode`: apply the transfer function once per source pixel in `ColorConverter` (luma and chroma passes each evaluate it today) and write 4:4:4 chroma without the `rg32float` intermediate; 4:4:4 10-bit at 4K takes 0.66 ms against 0.37 ms for 4:2:0
