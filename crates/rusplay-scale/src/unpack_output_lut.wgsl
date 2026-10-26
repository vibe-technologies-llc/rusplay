@group(2) @binding(1) var lut: texture_3d<f32>;
@group(2) @binding(2) var<uniform> lut_decoding: LutDecoding;

fn output_light(encoded: vec3<f32>) -> vec3<f32> {
    return lut_light(encoded, lut_decoding);
}
