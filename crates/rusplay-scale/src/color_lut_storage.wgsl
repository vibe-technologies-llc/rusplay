const LUT_GAMMA: f32 = 2.4;
const LUT_TOE: f32 = 0.0031308;
const LUT_TOE_SLOPE: f32 = 12.92;
const LUT_OFFSET: f32 = 0.055;

fn lut_encode(light: vec3<f32>, toe: u32) -> vec3<f32> {
    let magnitude = abs(light);
    let power = pow(magnitude, vec3<f32>(1.0 / LUT_GAMMA));
    if toe == 0u {
        return sign(light) * power;
    }
    let curve = (1.0 + LUT_OFFSET) * power - LUT_OFFSET;
    return sign(light) * select(curve, magnitude * LUT_TOE_SLOPE, magnitude <= vec3<f32>(LUT_TOE));
}

fn lut_decode(stored: vec3<f32>, toe: u32) -> vec3<f32> {
    let magnitude = abs(stored);
    if toe == 0u {
        return sign(stored) * pow(magnitude, vec3<f32>(LUT_GAMMA));
    }
    let curve = pow((magnitude + LUT_OFFSET) / (1.0 + LUT_OFFSET), vec3<f32>(LUT_GAMMA));
    return sign(stored) * select(curve, magnitude / LUT_TOE_SLOPE, magnitude <= vec3<f32>(LUT_TOE * LUT_TOE_SLOPE));
}
