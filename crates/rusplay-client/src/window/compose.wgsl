struct Rows {
    x: vec4<f32>,
    y: vec4<f32>,
    z: vec4<f32>,
}

struct Params {
    cursor_origin: vec2<i32>,
    cursor_size: vec2<i32>,
    cursor_to_target: Rows,
    target_to_signal: Rows,
    luma: vec4<f32>,
    levels: vec4<f32>,
    transfer: u32,
    dither: u32,
    noise_size: u32,
    bayer_bits: u32,
}

@group(0) @binding(0) var picture: texture_2d<f32>;
@group(0) @binding(1) var cursor: texture_2d<f32>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var noise: texture_2d<f32>;

const TRANSFER_LINEAR: u32 = 0u;
const TRANSFER_SRGB: u32 = 1u;
const TRANSFER_BT709: u32 = 2u;
const TRANSFER_GAMMA22: u32 = 3u;
const TRANSFER_PQ: u32 = 4u;
const TRANSFER_HLG: u32 = 5u;
const TRANSFER_EXTENDED_LINEAR: u32 = 6u;

const DITHER_ORDERED: u32 = 1u;
const DITHER_BLUE_NOISE: u32 = 2u;

const PQ_M1: f32 = 0.1593017578125;
const PQ_M2: f32 = 78.84375;
const PQ_C1: f32 = 0.8359375;
const PQ_C2: f32 = 18.8515625;
const PQ_C3: f32 = 18.6875;

const HLG_A: f32 = 0.17883277;
const HLG_B: f32 = 0.28466892;
const HLG_C: f32 = 0.55991073;

fn apply(m: Rows, v: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(dot(m.x.xyz, v), dot(m.y.xyz, v), dot(m.z.xyz, v));
}

fn srgb_oetf(x: vec3<f32>) -> vec3<f32> {
    return select(1.055 * pow(x, vec3<f32>(1.0 / 2.4)) - 0.055, x * 12.92, x <= vec3<f32>(0.0031308));
}

fn bt709_oetf(x: vec3<f32>) -> vec3<f32> {
    return select(1.099 * pow(x, vec3<f32>(0.45)) - 0.099, x * 4.5, x < vec3<f32>(0.018));
}

fn pq_oetf(normalized: vec3<f32>) -> vec3<f32> {
    let power = pow(normalized, vec3<f32>(PQ_M1));
    return pow((PQ_C1 + PQ_C2 * power) / (1.0 + PQ_C3 * power), vec3<f32>(PQ_M2));
}

fn hlg_oetf(scene: vec3<f32>) -> vec3<f32> {
    let curve = HLG_A * log(max(12.0 * scene - HLG_B, vec3<f32>(1e-6))) + HLG_C;
    return select(curve, sqrt(3.0 * scene), scene <= vec3<f32>(1.0 / 12.0));
}

fn hlg_encode(display: vec3<f32>) -> vec3<f32> {
    let luminance = dot(params.luma.xyz, display);
    if luminance <= 0.0 {
        return vec3<f32>(0.0);
    }
    let scene = clamp(display * pow(luminance, params.levels.y), vec3<f32>(0.0), vec3<f32>(1.0));
    return hlg_oetf(scene);
}

fn encode(light: vec3<f32>) -> vec3<f32> {
    let signal = apply(params.target_to_signal, light);
    let unit = clamp(signal, vec3<f32>(0.0), vec3<f32>(1.0));

    switch params.transfer {
        case TRANSFER_SRGB: {
            return srgb_oetf(unit);
        }
        case TRANSFER_BT709: {
            return bt709_oetf(unit);
        }
        case TRANSFER_GAMMA22: {
            return pow(unit, vec3<f32>(1.0 / 2.2));
        }
        case TRANSFER_PQ: {
            return pq_oetf(clamp(signal * params.levels.x, vec3<f32>(0.0), vec3<f32>(1.0)));
        }
        case TRANSFER_HLG: {
            return hlg_encode(max(signal * params.levels.x, vec3<f32>(0.0)));
        }
        case TRANSFER_EXTENDED_LINEAR: {
            return signal;
        }
        default: {
            return unit;
        }
    }
}

fn bayer_threshold(pixel: vec2<u32>) -> f32 {
    var rank = 0u;
    for (var bit = 0u; bit < params.bayer_bits; bit++) {
        let x_bit = (pixel.x >> bit) & 1u;
        let y_bit = (pixel.y >> bit) & 1u;
        rank = (rank << 2u) | (((x_bit ^ y_bit) << 1u) | y_bit);
    }
    return (f32(rank) + 0.5) / f32(1u << (2u * params.bayer_bits));
}

fn threshold(pixel: vec2<u32>) -> f32 {
    switch params.dither {
        case DITHER_ORDERED: {
            return bayer_threshold(pixel);
        }
        case DITHER_BLUE_NOISE: {
            return textureLoad(noise, vec2<i32>(pixel % params.noise_size), 0).r;
        }
        default: {
            return 0.5;
        }
    }
}

fn quantize(encoded: vec3<f32>, pixel: vec2<u32>) -> vec3<f32> {
    if params.dither == 0u {
        return encoded;
    }
    let steps = params.levels.z;
    let code = floor(clamp(encoded, vec3<f32>(0.0), vec3<f32>(1.0)) * steps + threshold(pixel));
    return min(code, vec3<f32>(steps)) / steps;
}

fn composed(pixel: vec2<i32>) -> vec3<f32> {
    let below = textureLoad(picture, pixel, 0).rgb;
    let local = pixel - params.cursor_origin;
    let inside = all(local >= vec2<i32>(0)) && all(local < params.cursor_size);

    if !inside {
        return below;
    }

    let over = textureLoad(cursor, local, 0);
    let coverage = clamp(over.a, 0.0, 1.0);
    return apply(params.cursor_to_target, max(over.rgb, vec3<f32>(0.0))) + below * (1.0 - coverage);
}

@vertex
fn cover(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(corner * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn compose(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(position.xy);
    let encoded = encode(composed(pixel));
    return vec4<f32>(quantize(encoded, vec2<u32>(pixel)), 1.0);
}
