struct Params {
    source_size: vec2<u32>,
    target_size: vec2<u32>,
    taps_per_pixel: u32,
    axis: u32,
    padding: vec2<u32>,
}

@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var destination: texture_storage_2d<rgba16float, write>;
@group(0) @binding(2) var<storage, read> firsts: array<i32>;
@group(0) @binding(3) var<storage, read> weights: array<f32>;
@group(0) @binding(4) var<uniform> params: Params;

@compute @workgroup_size(8, 8)
fn resample(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= params.target_size.x || id.y >= params.target_size.y {
        return;
    }

    let horizontal = params.axis == 0u;
    let output_index = select(id.y, id.x, horizontal);
    let source_last = i32(select(params.source_size.y, params.source_size.x, horizontal)) - 1;
    let first = firsts[output_index];

    var sum = vec4<f32>(0.0);
    for (var tap = 0u; tap < params.taps_per_pixel; tap++) {
        let weight = weights[output_index * params.taps_per_pixel + tap];
        let along = clamp(first + i32(tap), 0, source_last);
        let coord = select(vec2<i32>(i32(id.x), along), vec2<i32>(along, i32(id.y)), horizontal);
        sum += weight * textureLoad(source, coord, 0);
    }

    textureStore(destination, vec2<i32>(id.xy), sum);
}
