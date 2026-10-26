

# Colour
- [M] `client-color-lut`: bake the client's linearise, tone map and gamut map into a 65³ `rgba16float` 3D LUT over a PQ-shaped input (rebuilt when the source or target changes, tetrahedral lookup) and fuse it into the unpack pass; today an HDR frame takes three source-resolution passes (unpack, `Lineariser`, analytic `ColorMapper`), and perceptual HDR mapping alone is about 2 ms at 4K on an RX 9070
