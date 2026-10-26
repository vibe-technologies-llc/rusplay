struct Params {
    cursor_origin: vec2<i32>,
    cursor_size: vec2<i32>,
}

@group(0) @binding(0) var picture: texture_2d<f32>;
@group(0) @binding(1) var cursor: texture_2d<f32>;
@group(0) @binding(2) var<uniform> params: Params;

@vertex
fn cover(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(corner * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn compose(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(position.xy);
    let below = textureLoad(picture, pixel, 0).rgb;
    let local = pixel - params.cursor_origin;
    let inside = all(local >= vec2<i32>(0)) && all(local < params.cursor_size);

    if !inside {
        return vec4<f32>(below, 1.0);
    }

    let over = textureLoad(cursor, local, 0);
    let coverage = clamp(over.a, 0.0, 1.0);
    let colour = max(over.rgb, vec3<f32>(0.0)) + below * (1.0 - coverage);
    return vec4<f32>(colour, 1.0);
}
