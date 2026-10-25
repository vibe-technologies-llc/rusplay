struct Params {
    row_y: vec4<f32>,
    row_cb: vec4<f32>,
    row_cr: vec4<f32>,
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

fn encode_transfer(linear: vec3<f32>) -> vec3<f32> {
    let x = clamp(linear, vec3<f32>(0.0), vec3<f32>(1.0));
    switch params.transfer {
        case TRANSFER_SRGB: {
            return select(1.055 * pow(x, vec3<f32>(1.0 / 2.4)) - 0.055, 12.92 * x, x <= vec3<f32>(0.0031308));
        }
        case TRANSFER_BT709: {
            return select(1.099 * pow(x, vec3<f32>(0.45)) - 0.099, 4.5 * x, x < vec3<f32>(0.018));
        }
        default: {
            return x;
        }
    }
}

fn encoded_ycc(texel: vec4<f32>) -> vec3<f32> {
    let rgb = encode_transfer(texel.rgb);
    return vec3<f32>(
        dot(params.row_y.xyz, rgb),
        dot(params.row_cb.xyz, rgb),
        dot(params.row_cr.xyz, rgb),
    );
}

fn quantize(value: f32, quant: vec2<f32>) -> u32 {
    let code = clamp(round(value * quant.x + quant.y), 0.0, f32(params.max_code));
    return u32(code) << params.sample_shift;
}

fn samples_per_word() -> u32 {
    return 4u / params.sample_bytes;
}

fn sample_bit_offset(index: u32) -> u32 {
    return index * 8u * params.sample_bytes;
}
