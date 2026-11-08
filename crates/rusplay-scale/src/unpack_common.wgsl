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
    polar_ratio: vec2<f32>,
    polar_phase: vec2<f32>,
    tile_offset: vec2<i32>,
    tile_size: vec2<u32>,
    phase_rows: u32,
    antiringing: f32,
}

const TRANSFER_SRGB: u32 = 1u;
const TRANSFER_BT709: u32 = 2u;
const TRANSFER_GAMMA22: u32 = 3u;

fn decode_transfer(encoded: vec3<f32>) -> vec3<f32> {
    let x = clamp(encoded, vec3<f32>(0.0), vec3<f32>(1.0));
    switch unpacking.transfer {
        case TRANSFER_SRGB: {
            return select(pow((x + 0.055) / 1.055, vec3<f32>(2.4)), x / 12.92, x <= vec3<f32>(0.04045));
        }
        case TRANSFER_BT709: {
            return select(pow((x + 0.099) / 1.099, vec3<f32>(1.0 / 0.45)), x / 4.5, x < vec3<f32>(0.081));
        }
        case TRANSFER_GAMMA22: {
            return pow(x, vec3<f32>(2.2));
        }
        default: {
            return x;
        }
    }
}

fn dequantize(code: u32, quant: vec2<f32>) -> f32 {
    return (f32(code) - quant.y) / quant.x;
}

fn luma_at(x: u32, y: u32) -> f32 {
    return dequantize(luma_code(x, y), unpacking.luma_quant);
}

fn chroma_at(x: u32, y: u32) -> vec2<f32> {
    let codes = chroma_codes(x, y);
    return vec2<f32>(dequantize(codes.x, unpacking.chroma_quant), dequantize(codes.y, unpacking.chroma_quant));
}

fn antiring_chroma(sum: vec2<f32>, low: vec2<f32>, high: vec2<f32>) -> vec2<f32> {
    if unpacking.antiringing <= 0.0 || low.x > high.x {
        return sum;
    }

    return mix(sum, clamp(sum, low, high), unpacking.antiringing);
}
