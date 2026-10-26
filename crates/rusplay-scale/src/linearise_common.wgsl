struct LineariseParams {
    lanes: vec4<u32>,
    luma: vec4<f32>,
    hdr: vec4<f32>,
    transfer: u32,
    opaque: u32,
    reserved: vec2<u32>,
}

@group(2) @binding(0) var<uniform> params: LineariseParams;

@compute @workgroup_size(WORKGROUP_X, WORKGROUP_Y)
fn linearise(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(destination);
    if id.x >= size.x || id.y >= size.y {
        return;
    }

    var lanes = source_lanes(vec2<i32>(id.xy));
    let rgb = vec3<f32>(lanes[params.lanes.x], lanes[params.lanes.y], lanes[params.lanes.z]);
    let alpha = select(lanes[params.lanes.w], 1.0, params.opaque != 0u);
    let decoding = SignalDecoding(params.luma.xyz, params.hdr.x, params.hdr.y, params.transfer);
    textureStore(destination, vec2<i32>(id.xy), vec4<f32>(decode_signal(rgb, decoding), alpha));
}
