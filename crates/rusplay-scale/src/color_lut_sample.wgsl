struct LutDecoding {
    clamped: u32,
    peak: f32,
    toe: u32,
    reserved: u32,
}

fn lut_texel(base: vec3<i32>, offset: vec3<i32>) -> vec3<f32> {
    return textureLoad(lut, base + offset, 0).rgb;
}

fn lut_sample(code: vec3<f32>) -> vec3<f32> {
    let last = f32(textureDimensions(lut).x - 1u);
    let scaled = clamp(code, vec3<f32>(0.0), vec3<f32>(1.0)) * last;
    let corner = min(floor(scaled), vec3<f32>(last - 1.0));
    let f = scaled - corner;
    let base = vec3<i32>(corner);

    let origin = lut_texel(base, vec3<i32>(0, 0, 0));
    let far = lut_texel(base, vec3<i32>(1, 1, 1));
    if f.x >= f.y {
        if f.y >= f.z {
            return (1.0 - f.x) * origin + (f.x - f.y) * lut_texel(base, vec3<i32>(1, 0, 0)) + (f.y - f.z) * lut_texel(base, vec3<i32>(1, 1, 0)) + f.z * far;
        }
        if f.x >= f.z {
            return (1.0 - f.x) * origin + (f.x - f.z) * lut_texel(base, vec3<i32>(1, 0, 0)) + (f.z - f.y) * lut_texel(base, vec3<i32>(1, 0, 1)) + f.y * far;
        }
        return (1.0 - f.z) * origin + (f.z - f.x) * lut_texel(base, vec3<i32>(0, 0, 1)) + (f.x - f.y) * lut_texel(base, vec3<i32>(1, 0, 1)) + f.y * far;
    }
    if f.z >= f.y {
        return (1.0 - f.z) * origin + (f.z - f.y) * lut_texel(base, vec3<i32>(0, 0, 1)) + (f.y - f.x) * lut_texel(base, vec3<i32>(0, 1, 1)) + f.x * far;
    }
    if f.z >= f.x {
        return (1.0 - f.y) * origin + (f.y - f.z) * lut_texel(base, vec3<i32>(0, 1, 0)) + (f.z - f.x) * lut_texel(base, vec3<i32>(0, 1, 1)) + f.x * far;
    }
    return (1.0 - f.y) * origin + (f.y - f.x) * lut_texel(base, vec3<i32>(0, 1, 0)) + (f.x - f.z) * lut_texel(base, vec3<i32>(1, 1, 0)) + f.z * far;
}

fn lut_light(code: vec3<f32>, decoding: LutDecoding) -> vec3<f32> {
    let light = lut_decode(lut_sample(code), decoding.toe);
    if decoding.clamped != 0u {
        return clamp(light, vec3<f32>(0.0), vec3<f32>(decoding.peak));
    }
    return light;
}
