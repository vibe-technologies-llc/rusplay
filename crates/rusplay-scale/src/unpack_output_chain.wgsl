struct ChainSignal {
    luma: vec4<f32>,
    levels: vec4<f32>,
    transfer: u32,
    map: u32,
    toe: u32,
    reserved: u32,
}

@group(2) @binding(1) var<uniform> params: ColorParams;
@group(2) @binding(2) var<storage, read> state: ToneState;
@group(2) @binding(3) var boundary_table: texture_2d<f32>;
@group(2) @binding(4) var<uniform> chain: ChainSignal;

fn output_light(encoded: vec3<f32>) -> vec3<f32> {
    let decoding = SignalDecoding(chain.luma.xyz, chain.levels.x, chain.levels.y, chain.transfer);
    let light = decode_signal(encoded, decoding);
    if chain.map != 0u {
        return map_light(light);
    }
    return light;
}
