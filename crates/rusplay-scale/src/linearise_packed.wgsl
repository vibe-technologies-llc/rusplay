@group(0) @binding(0) var source: texture_2d<u32>;

const TEN_BIT_MASK: u32 = 1023u;
const TWO_BIT_MASK: u32 = 3u;

fn source_lanes(coord: vec2<i32>) -> vec4<f32> {
    let word = textureLoad(source, coord, 0).x;
    let ten = vec3<u32>(word >> 2u, word >> 12u, word >> 22u) & vec3<u32>(TEN_BIT_MASK);
    return vec4<f32>(vec3<f32>(ten) / f32(TEN_BIT_MASK), f32(word & TWO_BIT_MASK) / f32(TWO_BIT_MASK));
}
