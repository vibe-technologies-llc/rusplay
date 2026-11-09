struct Deband {
    directions: array<vec4<u32>, 4>,
    luma_threshold: f32,
    chroma_threshold: f32,
    iterations: u32,
    radius: u32,
    chroma_shift: vec2<u32>,
}

@group(1) @binding(0) var luma_target: texture_storage_2d<r32float, write>;
@group(1) @binding(1) var chroma_target: texture_storage_2d<rg32float, write>;
@group(2) @binding(0) var<uniform> unpacking: Params;
@group(2) @binding(1) var<uniform> deband: Deband;

const LUMA_PLANE: u32 = 0u;
const CHROMA_PLANE: u32 = 1u;
const ITERATION_SEEDS: u32 = 16u;
const DIRECTION_COUNT: u32 = 8u;
const DIRECTION_BITS: u32 = 12u;
const DIRECTION_SEED_SHIFT: u32 = 16u;

fn permute(value: u32) -> u32 {
    let state = value * 747796405u + 2891336453u;
    let word = ((state >> ((state >> 28u) + 4u)) ^ state) * 277803737u;
    return (word >> 22u) ^ word;
}

fn pair_offsets(pixel: vec2<u32>, plane: u32, iteration: u32, shift: vec2<u32>) -> vec4<i32> {
    let seed = permute(pixel.x + permute(pixel.y + permute(plane * ITERATION_SEEDS + iteration)));
    let low = deband.radius * (iteration - 1u) / deband.iterations + 1u;
    let high = max(deband.radius * iteration / deband.iterations, low);
    let reach = low + seed % (high - low + 1u);
    let direction = (seed >> DIRECTION_SEED_SHIFT) % DIRECTION_COUNT;
    let packed = deband.directions[direction / 2u];
    let unit = select(packed.xy, packed.zw, direction % 2u == 1u);
    let along = reach * unit.x;
    let across = reach * unit.y;
    let rounding = vec2<u32>(1u << (DIRECTION_BITS - 1u)) << shift;
    let bits = vec2<u32>(DIRECTION_BITS) + shift;
    let first = vec2<i32>((vec2<u32>(along, across) + rounding) >> bits);
    let second = vec2<i32>((vec2<u32>(across, along) + rounding) >> bits);
    return vec4<i32>(first, -second.x, second.y);
}

fn pair_weight(deviation: f32, threshold: f32) -> f32 {
    return clamp(2.0 - 2.0 * deviation / threshold, 0.0, 1.0);
}

fn offset_of(offsets: vec4<i32>, pair: u32) -> vec2<i32> {
    return select(offsets.xy, offsets.zw, pair == 1u);
}

fn luma_clamped(position: vec2<i32>) -> f32 {
    let clamped = vec2<u32>(clamp(position, vec2<i32>(0), vec2<i32>(unpacking.luma_size) - 1));
    return luma_at(clamped.x, clamped.y);
}

fn chroma_clamped(position: vec2<i32>) -> vec2<f32> {
    let clamped = vec2<u32>(clamp(position, vec2<i32>(0), vec2<i32>(unpacking.chroma_size) - 1));
    return chroma_at(clamped.x, clamped.y);
}

@compute @workgroup_size(WORKGROUP_X, WORKGROUP_Y)
fn deband_luma(@builtin(global_invocation_id) id: vec3<u32>) {
    if any(id.xy >= unpacking.luma_size) {
        return;
    }

    let pixel = vec2<i32>(id.xy);
    let centre = luma_at(id.x, id.y);
    let threshold = deband.luma_threshold;

    var sum = centre;
    var total = 1.0;
    if threshold > 0.0 {
        for (var iteration = 1u; iteration <= deband.iterations; iteration++) {
            let offsets = pair_offsets(id.xy, LUMA_PLANE, iteration, vec2<u32>(0u));
            for (var pair = 0u; pair < 2u; pair++) {
                let offset = offset_of(offsets, pair);
                let ahead = luma_clamped(pixel + offset);
                let behind = luma_clamped(pixel - offset);
                let weight = pair_weight(max(abs(ahead - centre), abs(behind - centre)), threshold);
                sum += weight * (ahead + behind);
                total += 2.0 * weight;
            }
        }
    }

    textureStore(luma_target, pixel, vec4<f32>(sum / total, 0.0, 0.0, 0.0));
}

@compute @workgroup_size(WORKGROUP_X, WORKGROUP_Y)
fn deband_chroma(@builtin(global_invocation_id) id: vec3<u32>) {
    if any(id.xy >= unpacking.chroma_size) {
        return;
    }

    let pixel = vec2<i32>(id.xy);
    let centre = chroma_at(id.x, id.y);
    let threshold = deband.chroma_threshold;

    var sum = centre;
    var total = 1.0;
    if threshold > 0.0 {
        for (var iteration = 1u; iteration <= deband.iterations; iteration++) {
            let offsets = pair_offsets(id.xy, CHROMA_PLANE, iteration, deband.chroma_shift);
            for (var pair = 0u; pair < 2u; pair++) {
                let offset = offset_of(offsets, pair);
                let ahead = chroma_clamped(pixel + offset);
                let behind = chroma_clamped(pixel - offset);
                let spread = max(abs(ahead - centre), abs(behind - centre));
                let weight = pair_weight(max(spread.x, spread.y), threshold);
                sum += weight * (ahead + behind);
                total += 2.0 * weight;
            }
        }
    }

    textureStore(chroma_target, pixel, vec4<f32>(sum / total, 0.0, 0.0));
}
