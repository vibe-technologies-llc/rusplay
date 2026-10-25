@group(0) @binding(0) var source: texture_2d<f32>;
@group(1) @binding(0) var<storage, read_write> output: array<u32>;
@group(2) @binding(0) var<uniform> params: Params;

@compute @workgroup_size(WORKGROUP_X, WORKGROUP_Y)
fn convert_direct(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= params.luma_stride || id.y >= params.luma_size.y {
        return;
    }

    let interleaved = params.interleaved != 0u;
    let per_word = samples_per_word();

    var luma_word = 0u;
    var chroma_words = array<u32, 2>(0u, 0u);
    var cr_word = 0u;
    for (var index = 0u; index < per_word; index++) {
        let x = id.x * per_word + index;
        if x >= params.luma_size.x {
            continue;
        }

        let ycc = encoded_ycc(textureLoad(source, vec2<i32>(i32(x), i32(id.y)), 0));
        let cb = quantize(ycc.y, params.chroma_quant);
        let cr = quantize(ycc.z, params.chroma_quant);

        luma_word |= quantize(ycc.x, params.luma_quant) << sample_bit_offset(index);
        if interleaved {
            let cb_sample = 2u * index;
            chroma_words[cb_sample / per_word] |= cb << sample_bit_offset(cb_sample % per_word);
            chroma_words[(cb_sample + 1u) / per_word] |= cr << sample_bit_offset((cb_sample + 1u) % per_word);
        } else {
            chroma_words[0] |= cb << sample_bit_offset(index);
            cr_word |= cr << sample_bit_offset(index);
        }
    }

    output[params.luma_offset + id.y * params.luma_stride + id.x] = luma_word;

    let chroma_row = id.y * params.chroma_stride;
    if interleaved {
        for (var part = 0u; part < 2u; part++) {
            let word = id.x * 2u + part;
            if word < params.chroma_stride {
                output[params.chroma_offsets.x + chroma_row + word] = chroma_words[part];
            }
        }
    } else {
        output[params.chroma_offsets.x + chroma_row + id.x] = chroma_words[0];
        output[params.chroma_offsets.y + chroma_row + id.x] = cr_word;
    }
}
