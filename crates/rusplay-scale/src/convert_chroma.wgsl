@group(0) @binding(0) var source: texture_2d<f32>;
@group(1) @binding(0) var chroma: texture_storage_2d<rg32float, write>;
@group(2) @binding(0) var<uniform> params: Params;
@group(2) @binding(1) var<storage, read> firsts: array<i32>;
@group(2) @binding(2) var<storage, read> weights: array<f32>;

@compute @workgroup_size(WORKGROUP_X, WORKGROUP_Y)
fn convert_chroma(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= params.chroma_size.x || id.y >= params.luma_size.y {
        return;
    }

    let source_last = i32(params.luma_size.x) - 1;
    let first = firsts[id.x];

    var sum = vec2<f32>(0.0);
    for (var tap = 0u; tap < params.horizontal_taps; tap++) {
        let weight = weights[id.x * params.horizontal_taps + tap];
        let x = clamp(first + i32(tap), 0, source_last);
        sum += weight * chroma_of(encoded_rgb(textureLoad(source, vec2<i32>(x, i32(id.y)), 0)));
    }

    textureStore(chroma, vec2<i32>(id.xy), vec4<f32>(sum, 0.0, 0.0));
}
