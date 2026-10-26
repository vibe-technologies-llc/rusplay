@group(1) @binding(0) var chroma: texture_2d<f32>;
@group(2) @binding(0) var target_texture: texture_storage_2d<rgba16float, write>;
@group(3) @binding(0) var<uniform> params: Params;
@group(3) @binding(1) var<storage, read> firsts: array<i32>;
@group(3) @binding(2) var<storage, read> weights: array<f32>;

fn filtered_chroma(x: u32, y: u32) -> vec2<f32> {
    let source_last = i32(params.chroma_size.y) - 1;
    let first = firsts[y];

    var sum = vec2<f32>(0.0);
    for (var tap = 0u; tap < params.vertical_taps; tap++) {
        let weight = weights[y * params.vertical_taps + tap];
        let row = clamp(first + i32(tap), 0, source_last);
        sum += weight * textureLoad(chroma, vec2<i32>(i32(x), row), 0).xy;
    }
    return sum;
}

@compute @workgroup_size(WORKGROUP_X, WORKGROUP_Y)
fn unpack_rgb(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= params.luma_size.x || id.y >= params.luma_size.y {
        return;
    }

    let ycbcr = vec3<f32>(luma_at(id.x, id.y), filtered_chroma(id.x, id.y));
    let encoded = vec3<f32>(dot(params.row_r.xyz, ycbcr), dot(params.row_g.xyz, ycbcr), dot(params.row_b.xyz, ycbcr));

    textureStore(target_texture, vec2<i32>(id.xy), vec4<f32>(output_light(encoded), 1.0));
}
