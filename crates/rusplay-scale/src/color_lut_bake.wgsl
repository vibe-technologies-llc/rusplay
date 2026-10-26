struct LutBake {
    luma: vec4<f32>,
    levels: vec4<f32>,
    transfer: u32,
    map: u32,
    toe: u32,
    reserved: u32,
}

@group(0) @binding(0) var lut_store: texture_storage_3d<rgba16float, write>;
@group(1) @binding(0) var<uniform> params: ColorParams;
@group(1) @binding(1) var<storage, read> state: ToneState;
@group(2) @binding(0) var<uniform> bake: LutBake;

@compute @workgroup_size(WORKGROUP_X, WORKGROUP_Y)
fn bake_lut(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(lut_store);
    if id.x >= size.x || id.y >= size.y || id.z >= size.z {
        return;
    }

    let code = vec3<f32>(id) / f32(size.x - 1u);
    let decoding = SignalDecoding(bake.luma.xyz, bake.levels.x, bake.levels.y, bake.transfer);
    var light = decode_signal(code, decoding);
    if bake.map != 0u {
        light = map_light(light);
    }
    textureStore(lut_store, vec3<i32>(id), vec4<f32>(lut_encode(light, bake.toe), 1.0));
}
