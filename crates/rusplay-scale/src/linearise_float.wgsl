@group(0) @binding(0) var source: texture_2d<f32>;

fn source_lanes(coord: vec2<i32>) -> vec4<f32> {
    return textureLoad(source, coord, 0);
}
