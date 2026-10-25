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
    hdr: vec4<f32>,
}

const TRANSFER_SRGB: u32 = 1u;
const TRANSFER_BT709: u32 = 2u;
const TRANSFER_PQ: u32 = 3u;
const TRANSFER_HLG: u32 = 4u;
const TRANSFER_GAMMA22: u32 = 5u;

const PQ_M1: f32 = 0.1593017578125;
const PQ_M2: f32 = 78.84375;
const PQ_C1: f32 = 0.8359375;
const PQ_C2: f32 = 18.8515625;
const PQ_C3: f32 = 18.6875;

const HLG_A: f32 = 0.17883277;
const HLG_B: f32 = 0.28466892;
const HLG_C: f32 = 0.55991073;
const HLG_KNEE: f32 = 0.08333333;

fn pq_oetf(display: vec3<f32>) -> vec3<f32> {
    let power = pow(display, vec3<f32>(PQ_M1));
    return pow((PQ_C1 + PQ_C2 * power) / (1.0 + PQ_C3 * power), vec3<f32>(PQ_M2));
}

fn hlg_oetf(scene: vec3<f32>) -> vec3<f32> {
    let curve = HLG_A * log(max(12.0 * scene - HLG_B, vec3<f32>(1.0e-6))) + HLG_C;
    return select(curve, sqrt(3.0 * scene), scene <= vec3<f32>(HLG_KNEE));
}

fn hlg_inverse_ootf(display: vec3<f32>) -> vec3<f32> {
    let luminance = dot(params.row_y.xyz, display);
    if luminance <= 0.0 {
        return vec3<f32>(0.0);
    }
    return clamp(display * pow(luminance, params.hdr.y), vec3<f32>(0.0), vec3<f32>(1.0));
}

fn encode_transfer(linear: vec3<f32>) -> vec3<f32> {
    let x = clamp(linear, vec3<f32>(0.0), vec3<f32>(1.0));
    switch params.transfer {
        case TRANSFER_SRGB: {
            return select(1.055 * pow(x, vec3<f32>(1.0 / 2.4)) - 0.055, 12.92 * x, x <= vec3<f32>(0.0031308));
        }
        case TRANSFER_BT709: {
            return select(1.099 * pow(x, vec3<f32>(0.45)) - 0.099, 4.5 * x, x < vec3<f32>(0.018));
        }
        case TRANSFER_GAMMA22: {
            return pow(x, vec3<f32>(1.0 / 2.2));
        }
        case TRANSFER_PQ: {
            return pq_oetf(clamp(linear * params.hdr.x, vec3<f32>(0.0), vec3<f32>(1.0)));
        }
        case TRANSFER_HLG: {
            return hlg_oetf(hlg_inverse_ootf(clamp(linear * params.hdr.x, vec3<f32>(0.0), vec3<f32>(1.0))));
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
