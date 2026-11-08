@group(1) @binding(0) var chroma: texture_storage_2d<rg32float, write>;
@group(2) @binding(0) var<uniform> unpacking: Params;
@group(2) @binding(1) var<storage, read> offsets: array<i32>;
@group(2) @binding(2) var<storage, read> weights: array<f32>;

override POLAR_CHROMA_TILE: u32 = 24u;

var<workgroup> tile: array<vec2<f32>, POLAR_CHROMA_TILE * POLAR_CHROMA_TILE>;

fn sample_anchor(pixel: vec2<u32>) -> vec2<i32> {
    return vec2<i32>(floor((vec2<f32>(pixel) + 0.5) * unpacking.polar_ratio - 0.5 + unpacking.polar_phase));
}

fn load_tile(origin: vec2<i32>, local: u32) {
    let source_last = vec2<i32>(unpacking.chroma_size) - 1;
    let width = unpacking.tile_size.x;
    for (var index = local; index < width * unpacking.tile_size.y; index += WORKGROUP_X * WORKGROUP_Y) {
        let position = origin + vec2<i32>(i32(index % width), i32(index / width));
        let clamped = vec2<u32>(clamp(position, vec2<i32>(0), source_last));
        tile[index] = chroma_at(clamped.x, clamped.y);
    }
}

@compute @workgroup_size(WORKGROUP_X, WORKGROUP_Y)
fn unpack_chroma_polar(
    @builtin(global_invocation_id) id: vec3<u32>,
    @builtin(workgroup_id) group: vec3<u32>,
    @builtin(local_invocation_index) local: u32,
) {
    let origin = sample_anchor(group.xy * vec2<u32>(WORKGROUP_X, WORKGROUP_Y)) + unpacking.tile_offset;

    load_tile(origin, local);
    workgroupBarrier();

    if any(id.xy >= unpacking.luma_size) {
        return;
    }

    let anchor = sample_anchor(id.xy) - origin;
    let phase = id.x % 2u + 2u * (id.y % unpacking.phase_rows);
    let first = phase * unpacking.horizontal_taps;

    var sum = vec2<f32>(0.0);
    var nearest_low = vec2<f32>(3.0e38);
    var nearest_high = vec2<f32>(-3.0e38);
    for (var tap = first; tap < first + unpacking.horizontal_taps; tap++) {
        let offset = vec2<i32>(offsets[2u * tap], offsets[2u * tap + 1u]);
        let position = vec2<u32>(anchor + offset);
        let texel = tile[position.y * unpacking.tile_size.x + position.x];
        sum += weights[tap] * texel;

        if all(offset >= vec2<i32>(0)) && all(offset <= vec2<i32>(1)) {
            nearest_low = min(nearest_low, texel);
            nearest_high = max(nearest_high, texel);
        }
    }

    textureStore(chroma, vec2<i32>(id.xy), vec4<f32>(antiring_chroma(sum, nearest_low, nearest_high), 0.0, 0.0));
}
