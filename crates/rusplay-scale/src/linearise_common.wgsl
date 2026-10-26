struct LineariseParams {
    lanes: vec4<u32>,
    luma: vec4<f32>,
    hdr: vec4<f32>,
    transfer: u32,
    opaque: u32,
    reserved: vec2<u32>,
}

@group(2) @binding(0) var<uniform> params: LineariseParams;

const TRANSFER_NONE: u32 = 0u;
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

fn pq_eotf(code: vec3<f32>) -> vec3<f32> {
    let power = pow(code, vec3<f32>(1.0 / PQ_M2));
    return pow(max(power - PQ_C1, vec3<f32>(0.0)) / (PQ_C2 - PQ_C3 * power), vec3<f32>(1.0 / PQ_M1));
}

fn hlg_inverse_oetf(code: vec3<f32>) -> vec3<f32> {
    let curve = (exp((code - HLG_C) / HLG_A) + HLG_B) / 12.0;
    return select(curve, code * code / 3.0, code <= vec3<f32>(0.5));
}

fn hlg_ootf(scene: vec3<f32>) -> vec3<f32> {
    let luminance = dot(params.luma.xyz, scene);
    if luminance <= 0.0 {
        return vec3<f32>(0.0);
    }
    return scene * pow(luminance, params.hdr.y);
}

fn decode_transfer(encoded: vec3<f32>) -> vec3<f32> {
    let x = clamp(encoded, vec3<f32>(0.0), vec3<f32>(1.0));
    switch params.transfer {
        case TRANSFER_SRGB: {
            return select(pow((x + 0.055) / 1.055, vec3<f32>(2.4)), x / 12.92, x <= vec3<f32>(0.04045));
        }
        case TRANSFER_BT709: {
            return select(pow((x + 0.099) / 1.099, vec3<f32>(1.0 / 0.45)), x / 4.5, x < vec3<f32>(0.081));
        }
        case TRANSFER_GAMMA22: {
            return pow(x, vec3<f32>(2.2));
        }
        case TRANSFER_PQ: {
            return pq_eotf(x) * params.hdr.x;
        }
        case TRANSFER_HLG: {
            return hlg_ootf(hlg_inverse_oetf(x)) * params.hdr.x;
        }
        default: {
            return encoded;
        }
    }
}

@compute @workgroup_size(WORKGROUP_X, WORKGROUP_Y)
fn linearise(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(destination);
    if id.x >= size.x || id.y >= size.y {
        return;
    }

    var lanes = source_lanes(vec2<i32>(id.xy));
    let rgb = vec3<f32>(lanes[params.lanes.x], lanes[params.lanes.y], lanes[params.lanes.z]);
    let alpha = select(lanes[params.lanes.w], 1.0, params.opaque != 0u);
    textureStore(destination, vec2<i32>(id.xy), vec4<f32>(decode_transfer(rgb), alpha));
}
