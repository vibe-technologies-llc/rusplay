struct Shaping {
    antiringing: f32,
    sigmoid_stages: u32,
    sigmoid_center: f32,
    sigmoid_slope: f32,
    sigmoid_offset: f32,
    sigmoid_scale: f32,
    padding: vec2<u32>,
}

const SIGMOIDIZE_INPUT: u32 = 1u;
const UNSIGMOIDIZE_OUTPUT: u32 = 2u;

fn shape_input(shaping: Shaping, texel: vec4<f32>) -> vec4<f32> {
    if (shaping.sigmoid_stages & SIGMOIDIZE_INPUT) == 0u {
        return texel;
    }

    let linear = clamp(texel.rgb, vec3<f32>(0.0), vec3<f32>(1.0));
    let shaped = shaping.sigmoid_center
        - log(1.0 / (linear * shaping.sigmoid_scale + shaping.sigmoid_offset) - 1.0)
        / shaping.sigmoid_slope;
    return vec4<f32>(shaped, texel.a);
}

fn shape_output(shaping: Shaping, color: vec4<f32>) -> vec4<f32> {
    if (shaping.sigmoid_stages & UNSIGMOIDIZE_OUTPUT) == 0u {
        return color;
    }

    let logistic = 1.0 / (1.0 + exp(shaping.sigmoid_slope * (shaping.sigmoid_center - color.rgb)));
    return vec4<f32>((logistic - shaping.sigmoid_offset) / shaping.sigmoid_scale, color.a);
}

fn antiring(shaping: Shaping, color: vec4<f32>, low: vec4<f32>, high: vec4<f32>) -> vec4<f32> {
    if shaping.antiringing <= 0.0 || low.x > high.x {
        return color;
    }

    return mix(color, clamp(color, low, high), shaping.antiringing);
}
