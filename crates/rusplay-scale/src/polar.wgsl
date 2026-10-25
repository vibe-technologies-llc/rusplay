struct Params {
    shaping: Shaping,
    source_size: vec2<u32>,
    target_size: vec2<u32>,
    ratio: vec2<f32>,
    stretch: vec2<f32>,
    radius: f32,
    lut_size: u32,
    phase: vec2<f32>,
}

@group(0) @binding(0) var source: texture_2d<f32>;
@group(2) @binding(0) var<storage, read> radial_lut: array<f32>;
@group(2) @binding(1) var<uniform> params: Params;

fn radial_weight(distance: f32) -> f32 {
    let position = distance / params.radius * f32(params.lut_size - 1u);
    let index = min(u32(position), params.lut_size - 1u);
    let next = min(index + 1u, params.lut_size - 1u);
    return mix(radial_lut[index], radial_lut[next], position - f32(index));
}

@compute @workgroup_size(WORKGROUP_X, WORKGROUP_Y)
fn resample_polar(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= params.target_size.x || id.y >= params.target_size.y {
        return;
    }

    let center = (vec2<f32>(id.xy) + 0.5) * params.ratio - 0.5 + params.phase;
    let extent = params.radius * params.stretch;
    let first = vec2<i32>(floor(center - extent)) + 1;
    let last = vec2<i32>(ceil(center + extent)) - 1;
    let anchor = vec2<i32>(floor(center));
    let source_last = vec2<i32>(params.source_size) - 1;

    var sum = vec4<f32>(0.0);
    var total = 0.0;
    var nearest_low = vec4<f32>(3.0e38);
    var nearest_high = vec4<f32>(-3.0e38);
    for (var y = first.y; y <= last.y; y++) {
        for (var x = first.x; x <= last.x; x++) {
            let position = vec2<i32>(x, y);
            let distance = length((vec2<f32>(position) - center) / params.stretch);
            if distance >= params.radius {
                continue;
            }

            let weight = radial_weight(distance);
            let texel = shape_input(params.shaping, textureLoad(source, clamp(position, vec2<i32>(0), source_last), 0));
            sum += weight * texel;
            total += weight;

            let offset = position - anchor;
            if all(offset >= vec2<i32>(0)) && all(offset <= vec2<i32>(1)) {
                nearest_low = min(nearest_low, texel);
                nearest_high = max(nearest_high, texel);
            }
        }
    }

    let color = antiring(params.shaping, sum / total, nearest_low, nearest_high);
    textureStore(destination, vec2<i32>(id.xy), shape_output(params.shaping, color));
}
