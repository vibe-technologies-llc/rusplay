const SIGNAL_NONE: u32 = 0u;
const SIGNAL_SRGB: u32 = 1u;
const SIGNAL_BT709: u32 = 2u;
const SIGNAL_PQ: u32 = 3u;
const SIGNAL_HLG: u32 = 4u;
const SIGNAL_GAMMA22: u32 = 5u;

const PQ_M1: f32 = 0.1593017578125;
const PQ_M2: f32 = 78.84375;
const PQ_C1: f32 = 0.8359375;
const PQ_C2: f32 = 18.8515625;
const PQ_C3: f32 = 18.6875;

const HLG_A: f32 = 0.17883277;
const HLG_B: f32 = 0.28466892;
const HLG_C: f32 = 0.55991073;

struct SignalDecoding {
    luma: vec3<f32>,
    scale: f32,
    hlg_exponent: f32,
    transfer: u32,
}

fn pq_eotf(code: vec3<f32>) -> vec3<f32> {
    let power = pow(code, vec3<f32>(1.0 / PQ_M2));
    return pow(max(power - PQ_C1, vec3<f32>(0.0)) / (PQ_C2 - PQ_C3 * power), vec3<f32>(1.0 / PQ_M1));
}

fn hlg_inverse_oetf(code: vec3<f32>) -> vec3<f32> {
    let curve = (exp((code - HLG_C) / HLG_A) + HLG_B) / 12.0;
    return select(curve, code * code / 3.0, code <= vec3<f32>(0.5));
}

fn hlg_ootf(scene: vec3<f32>, decoding: SignalDecoding) -> vec3<f32> {
    let luminance = dot(decoding.luma, scene);
    if luminance <= 0.0 {
        return vec3<f32>(0.0);
    }
    return scene * pow(luminance, decoding.hlg_exponent);
}

fn decode_signal(encoded: vec3<f32>, decoding: SignalDecoding) -> vec3<f32> {
    let x = clamp(encoded, vec3<f32>(0.0), vec3<f32>(1.0));
    switch decoding.transfer {
        case SIGNAL_SRGB: {
            return select(pow((x + 0.055) / 1.055, vec3<f32>(2.4)), x / 12.92, x <= vec3<f32>(0.04045));
        }
        case SIGNAL_BT709: {
            return select(pow((x + 0.099) / 1.099, vec3<f32>(1.0 / 0.45)), x / 4.5, x < vec3<f32>(0.081));
        }
        case SIGNAL_GAMMA22: {
            return pow(x, vec3<f32>(2.2));
        }
        case SIGNAL_PQ: {
            return pq_eotf(x) * decoding.scale;
        }
        case SIGNAL_HLG: {
            return hlg_ootf(hlg_inverse_oetf(x), decoding) * decoding.scale;
        }
        default: {
            return encoded;
        }
    }
}
