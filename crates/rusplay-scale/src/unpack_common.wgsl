struct Params {
    row_r: vec4<f32>,
    row_g: vec4<f32>,
    row_b: vec4<f32>,
    luma_quant: vec2<f32>,
    chroma_quant: vec2<f32>,
    luma_size: vec2<u32>,
    chroma_size: vec2<u32>,
    transfer: u32,
    sample_bytes: u32,
    sample_shift: u32,
    max_code: u32,
    luma_offset: u32,
    luma_stride: u32,
    chroma_offsets: vec2<u32>,
    chroma_stride: u32,
    interleaved: u32,
    horizontal_taps: u32,
    vertical_taps: u32,
}

const TRANSFER_SRGB: u32 = 1u;
const TRANSFER_BT709: u32 = 2u;

fn decode_transfer(encoded: vec3<f32>) -> vec3<f32> {
    let x = clamp(encoded, vec3<f32>(0.0), vec3<f32>(1.0));
    switch params.transfer {
        case TRANSFER_SRGB: {
            return select(pow((x + 0.055) / 1.055, vec3<f32>(2.4)), x / 12.92, x <= vec3<f32>(0.04045));
        }
        case TRANSFER_BT709: {
            return select(pow((x + 0.099) / 1.099, vec3<f32>(1.0 / 0.45)), x / 4.5, x < vec3<f32>(0.081));
        }
        default: {
            return x;
        }
    }
}

fn sample_at(plane_offset: u32, stride: u32, index: u32, row: u32) -> u32 {
    let byte = index * params.sample_bytes;
    let word = input[plane_offset + row * stride + byte / 4u];
    let mask = select(0xffu, 0xffffu, params.sample_bytes == 2u);
    let bits = (word >> ((byte % 4u) * 8u)) & mask;
    return min(bits >> params.sample_shift, params.max_code);
}

fn dequantize(code: u32, quant: vec2<f32>) -> f32 {
    return (f32(code) - quant.y) / quant.x;
}

fn luma_at(x: u32, y: u32) -> f32 {
    return dequantize(sample_at(params.luma_offset, params.luma_stride, x, y), params.luma_quant);
}

fn chroma_at(x: u32, y: u32) -> vec2<f32> {
    if params.interleaved != 0u {
        let cb = sample_at(params.chroma_offsets.x, params.chroma_stride, 2u * x, y);
        let cr = sample_at(params.chroma_offsets.x, params.chroma_stride, 2u * x + 1u, y);
        return vec2<f32>(dequantize(cb, params.chroma_quant), dequantize(cr, params.chroma_quant));
    }

    let cb = sample_at(params.chroma_offsets.x, params.chroma_stride, x, y);
    let cr = sample_at(params.chroma_offsets.y, params.chroma_stride, x, y);
    return vec2<f32>(dequantize(cb, params.chroma_quant), dequantize(cr, params.chroma_quant));
}
