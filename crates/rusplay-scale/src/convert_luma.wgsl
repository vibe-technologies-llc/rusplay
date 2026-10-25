@group(0) @binding(0) var source: texture_2d<f32>;
@group(1) @binding(0) var<storage, read_write> output: array<u32>;
@group(2) @binding(0) var<uniform> params: Params;

@compute @workgroup_size(WORKGROUP_X, WORKGROUP_Y)
fn convert_luma(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= params.luma_stride || id.y >= params.luma_size.y {
        return;
    }

    var word = 0u;
    for (var index = 0u; index < samples_per_word(); index++) {
        let x = id.x * samples_per_word() + index;
        if x < params.luma_size.x {
            let rgb = encoded_rgb(textureLoad(source, vec2<i32>(i32(x), i32(id.y)), 0));
            word |= quantize(luma_of(rgb), params.luma_quant) << sample_bit_offset(index);
        }
    }

    output[params.luma_offset + id.y * params.luma_stride + id.x] = word;
}
