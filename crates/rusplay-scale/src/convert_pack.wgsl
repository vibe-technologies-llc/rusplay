@group(0) @binding(0) var chroma: texture_2d<f32>;
@group(1) @binding(0) var<storage, read_write> output: array<u32>;
@group(2) @binding(0) var<uniform> params: Params;
@group(2) @binding(1) var<storage, read> firsts: array<i32>;
@group(2) @binding(2) var<storage, read> weights: array<f32>;

fn filtered_chroma(x: u32, y: u32) -> vec2<f32> {
    let source_last = i32(params.luma_size.y) - 1;
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
fn pack_chroma(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= params.chroma_stride || id.y >= params.chroma_size.y {
        return;
    }

    let interleaved = params.interleaved != 0u;

    var word = 0u;
    for (var index = 0u; index < samples_per_word(); index++) {
        let sample = id.x * samples_per_word() + index;
        let x = select(sample, sample / 2u, interleaved);
        let component = select(id.z, sample % 2u, interleaved);
        if x < params.chroma_size.x {
            let value = filtered_chroma(x, id.y)[component];
            word |= quantize(value, params.chroma_quant) << sample_bit_offset(index);
        }
    }

    let plane_offset = select(params.chroma_offsets.x, params.chroma_offsets.y, id.z == 1u);
    output[plane_offset + id.y * params.chroma_stride + id.x] = word;
}
