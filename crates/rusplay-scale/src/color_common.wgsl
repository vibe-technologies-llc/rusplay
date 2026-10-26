struct Rows {
    x: vec4<f32>,
    y: vec4<f32>,
    z: vec4<f32>,
}

struct ColorParams {
    source_to_target: Rows,
    source_to_work: Rows,
    work_to_target: Rows,
    work_to_source: Rows,
    xyz_to_lms: Rows,
    lms_to_xyz: Rows,
    lms_to_lab: Rows,
    lab_to_lms: Rows,
    tone_xyz_to_lms: Rows,
    tone_lms_to_xyz: Rows,
    tone_lms_to_itp: Rows,
    tone_itp_to_lms: Rows,
    target_luma: vec4<f32>,
    levels: vec4<f32>,
    tone_levels: vec4<f32>,
    peak_limits: vec4<f32>,
    dynamic: vec4<f32>,
    chroma_limits: vec4<f32>,
    path: u32,
    space: u32,
    tone: u32,
    dynamic_peak: u32,
}

const SPACE_ICTCP: u32 = 1u;

const COLOR_PQ_M1: f32 = 0.1593017578125;
const COLOR_PQ_M2: f32 = 78.84375;
const COLOR_PQ_C1: f32 = 0.8359375;
const COLOR_PQ_C2: f32 = 18.8515625;
const COLOR_PQ_C3: f32 = 18.6875;

fn apply(m: Rows, v: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(dot(m.x.xyz, v), dot(m.y.xyz, v), dot(m.z.xyz, v));
}

fn pq_encode(normalized: vec3<f32>) -> vec3<f32> {
    let power = pow(clamp(normalized, vec3<f32>(0.0), vec3<f32>(1.0)), vec3<f32>(COLOR_PQ_M1));
    return pow((COLOR_PQ_C1 + COLOR_PQ_C2 * power) / (1.0 + COLOR_PQ_C3 * power), vec3<f32>(COLOR_PQ_M2));
}

fn pq_decode(code: vec3<f32>) -> vec3<f32> {
    let power = pow(clamp(code, vec3<f32>(0.0), vec3<f32>(1.0)), vec3<f32>(1.0 / COLOR_PQ_M2));
    return pow(max(power - COLOR_PQ_C1, vec3<f32>(0.0)) / (COLOR_PQ_C2 - COLOR_PQ_C3 * power), vec3<f32>(1.0 / COLOR_PQ_M1));
}

fn signed_pq_encode(normalized: vec3<f32>) -> vec3<f32> {
    return sign(normalized) * pq_encode(abs(normalized));
}

fn signed_pq_decode(code: vec3<f32>) -> vec3<f32> {
    return sign(code) * pq_decode(abs(code));
}

fn signed_cbrt(value: vec3<f32>) -> vec3<f32> {
    return sign(value) * pow(abs(value), vec3<f32>(1.0 / 3.0));
}

fn xyz_to_lab(xyz: vec3<f32>) -> vec3<f32> {
    let lms = apply(params.xyz_to_lms, xyz);
    var shaped: vec3<f32>;
    if params.space == SPACE_ICTCP {
        shaped = signed_pq_encode(lms * params.levels.z);
    } else {
        shaped = signed_cbrt(lms);
    }
    return apply(params.lms_to_lab, shaped);
}

fn lab_to_xyz(lab: vec3<f32>) -> vec3<f32> {
    let shaped = apply(params.lab_to_lms, lab);
    var lms: vec3<f32>;
    if params.space == SPACE_ICTCP {
        lms = signed_pq_decode(shaped) / params.levels.z;
    } else {
        lms = shaped * shaped * shaped;
    }
    return apply(params.lms_to_xyz, lms);
}
