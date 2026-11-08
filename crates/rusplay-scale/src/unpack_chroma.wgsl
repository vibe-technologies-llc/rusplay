@group(1) @binding(0) var chroma: texture_storage_2d<rg32float, write>;
@group(2) @binding(0) var<uniform> unpacking: Params;
@group(2) @binding(1) var<storage, read> firsts: array<i32>;
@group(2) @binding(2) var<storage, read> weights: array<f32>;
@group(2) @binding(3) var<storage, read> anchors: array<i32>;

@compute @workgroup_size(WORKGROUP_X, WORKGROUP_Y)
fn unpack_chroma(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= unpacking.luma_size.x || id.y >= unpacking.chroma_size.y {
        return;
    }

    let source_last = i32(unpacking.chroma_size.x) - 1;
    let first = firsts[id.x];
    let anchor = anchors[id.x];

    var sum = vec2<f32>(0.0);
    var nearest_low = vec2<f32>(3.0e38);
    var nearest_high = vec2<f32>(-3.0e38);
    for (var tap = 0u; tap < unpacking.horizontal_taps; tap++) {
        let weight = weights[id.x * unpacking.horizontal_taps + tap];
        let position = first + i32(tap);
        let texel = chroma_at(u32(clamp(position, 0, source_last)), id.y);
        sum += weight * texel;

        let offset = position - anchor;
        if offset == 0 || offset == 1 {
            nearest_low = min(nearest_low, texel);
            nearest_high = max(nearest_high, texel);
        }
    }

    textureStore(chroma, vec2<i32>(id.xy), vec4<f32>(antiring_chroma(sum, nearest_low, nearest_high), 0.0, 0.0));
}
