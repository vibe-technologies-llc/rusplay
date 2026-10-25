@group(0) @binding(0) var luma_plane: texture_2d<u32>;
@group(0) @binding(1) var chroma_plane: texture_2d<u32>;

fn luma_code(x: u32, y: u32) -> u32 {
    let bits = textureLoad(luma_plane, vec2<u32>(x, y), 0).x;
    return min(bits >> params.sample_shift, params.max_code);
}

fn chroma_codes(x: u32, y: u32) -> vec2<u32> {
    let bits = textureLoad(chroma_plane, vec2<u32>(x, y), 0).xy;
    return min(bits >> vec2<u32>(params.sample_shift), vec2<u32>(params.max_code));
}
