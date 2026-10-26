fn map_light(rgb: vec3<f32>) -> vec3<f32> {
    var light: vec3<f32>;
    if params.tone != 0u {
        light = tone_map(rgb, state.curve);
    } else {
        light = rgb * params.levels.w;
    }
    return map_gamut(light);
}
