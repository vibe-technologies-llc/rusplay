@group(0) @binding(0) var<storage, read> input: array<u32>;

fn sample_at(plane_offset: u32, stride: u32, index: u32, row: u32) -> u32 {
    let byte = index * params.sample_bytes;
    let word = input[plane_offset + row * stride + byte / 4u];
    let mask = select(0xffu, 0xffffu, params.sample_bytes == 2u);
    let bits = (word >> ((byte % 4u) * 8u)) & mask;
    return min(bits >> params.sample_shift, params.max_code);
}

fn luma_code(x: u32, y: u32) -> u32 {
    return sample_at(params.luma_offset, params.luma_stride, x, y);
}

fn chroma_codes(x: u32, y: u32) -> vec2<u32> {
    if params.interleaved != 0u {
        let cb = sample_at(params.chroma_offsets.x, params.chroma_stride, 2u * x, y);
        let cr = sample_at(params.chroma_offsets.x, params.chroma_stride, 2u * x + 1u, y);
        return vec2<u32>(cb, cr);
    }

    let cb = sample_at(params.chroma_offsets.x, params.chroma_stride, x, y);
    let cr = sample_at(params.chroma_offsets.y, params.chroma_stride, x, y);
    return vec2<u32>(cb, cr);
}
