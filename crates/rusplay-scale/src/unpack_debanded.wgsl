@group(0) @binding(0) var debanded_luma: texture_2d<f32>;
@group(0) @binding(1) var debanded_chroma: texture_2d<f32>;

fn luma_at(x: u32, y: u32) -> f32 {
    return textureLoad(debanded_luma, vec2<u32>(x, y), 0).x;
}

fn chroma_at(x: u32, y: u32) -> vec2<f32> {
    return textureLoad(debanded_chroma, vec2<u32>(x, y), 0).xy;
}
