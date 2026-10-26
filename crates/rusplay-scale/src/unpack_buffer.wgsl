@group(0) @binding(0) var<storage, read> input: array<u32>;

fn sample_at(plane_offset: u32, stride: u32, index: u32, row: u32) -> u32 {
    let byte = index * unpacking.sample_bytes;
    let word = input[plane_offset + row * stride + byte / 4u];
    let mask = select(0xffu, 0xffffu, unpacking.sample_bytes == 2u);
    let bits = (word >> ((byte % 4u) * 8u)) & mask;
    return min(bits >> unpacking.sample_shift, unpacking.max_code);
}

fn luma_code(x: u32, y: u32) -> u32 {
    return sample_at(unpacking.luma_offset, unpacking.luma_stride, x, y);
}

fn chroma_codes(x: u32, y: u32) -> vec2<u32> {
    if unpacking.interleaved != 0u {
        let cb = sample_at(unpacking.chroma_offsets.x, unpacking.chroma_stride, 2u * x, y);
        let cr = sample_at(unpacking.chroma_offsets.x, unpacking.chroma_stride, 2u * x + 1u, y);
        return vec2<u32>(cb, cr);
    }

    let cb = sample_at(unpacking.chroma_offsets.x, unpacking.chroma_stride, x, y);
    let cr = sample_at(unpacking.chroma_offsets.y, unpacking.chroma_stride, x, y);
    return vec2<u32>(cb, cr);
}
