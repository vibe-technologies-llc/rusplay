@group(1) @binding(0) var chroma: texture_2d<f32>;
@group(2) @binding(0) var target_texture: texture_storage_2d<rgba16float, write>;
@group(3) @binding(0) var<uniform> unpacking: Params;
@group(3) @binding(1) var<storage, read> firsts: array<i32>;
@group(3) @binding(2) var<storage, read> weights: array<f32>;
@group(3) @binding(3) var<storage, read> anchors: array<i32>;

fn filtered_chroma(x: u32, y: u32) -> vec2<f32> {
    if all(unpacking.chroma_size == unpacking.luma_size) {
        return chroma_at(x, y);
    }
    let source_last = i32(textureDimensions(chroma).y) - 1;
    let first = firsts[y];
    let anchor = anchors[y];

    var sum = vec2<f32>(0.0);
    var nearest_low = vec2<f32>(3.0e38);
    var nearest_high = vec2<f32>(-3.0e38);
    for (var tap = 0u; tap < unpacking.vertical_taps; tap++) {
        let weight = weights[y * unpacking.vertical_taps + tap];
        let position = first + i32(tap);
        let texel = textureLoad(chroma, vec2<i32>(i32(x), clamp(position, 0, source_last)), 0).xy;
        sum += weight * texel;

        let offset = position - anchor;
        if offset == 0 || offset == 1 {
            nearest_low = min(nearest_low, texel);
            nearest_high = max(nearest_high, texel);
        }
    }
    return antiring_chroma(sum, nearest_low, nearest_high);
}

@compute @workgroup_size(WORKGROUP_X, WORKGROUP_Y)
fn unpack_rgb(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= unpacking.luma_size.x || id.y >= unpacking.luma_size.y {
        return;
    }

    let ycbcr = vec3<f32>(luma_at(id.x, id.y), filtered_chroma(id.x, id.y));
    let encoded = vec3<f32>(dot(unpacking.row_r.xyz, ycbcr), dot(unpacking.row_g.xyz, ycbcr), dot(unpacking.row_b.xyz, ycbcr));

    textureStore(target_texture, vec2<i32>(id.xy), vec4<f32>(output_light(encoded), 1.0));
}
