@group(0) @binding(0) var source: texture_2d<f32>;
@group(1) @binding(0) var destination: texture_storage_2d<rgba16float, write>;
@group(2) @binding(0) var<uniform> params: ColorParams;
@group(2) @binding(1) var<storage, read> state: ToneState;

@compute @workgroup_size(WORKGROUP_X, WORKGROUP_Y)
fn map_color(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(destination);
    if id.x >= size.x || id.y >= size.y {
        return;
    }

    let texel = textureLoad(source, vec2<i32>(id.xy), 0);
    var light: vec3<f32>;
    if params.tone != 0u {
        light = tone_map(texel.rgb, state.curve);
    } else {
        light = texel.rgb * params.levels.w;
    }
    textureStore(destination, vec2<i32>(id.xy), vec4<f32>(map_gamut(light), texel.a));
}
