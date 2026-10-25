override HISTOGRAM_BINS: u32;

@group(0) @binding(0) var source: texture_2d<f32>;
@group(1) @binding(0) var<uniform> params: ColorParams;
@group(1) @binding(1) var<storage, read_write> state: ToneState;
@group(1) @binding(2) var<storage, read_write> histogram: array<atomic<u32>>;

var<workgroup> local_bins: array<atomic<u32>, HISTOGRAM_BINS>;

@compute @workgroup_size(WORKGROUP_X, WORKGROUP_Y)
fn measure_histogram(
    @builtin(global_invocation_id) id: vec3<u32>,
    @builtin(local_invocation_index) local: u32,
) {
    atomicStore(&local_bins[local], 0u);
    workgroupBarrier();

    let size = textureDimensions(source);
    if id.x < size.x && id.y < size.y {
        let rgb = textureLoad(source, vec2<i32>(id.xy), 0).rgb;
        let nits = max(max(rgb.r, rgb.g), max(rgb.b, 0.0)) * params.tone_levels.x;
        let code = pq_from_nits(nits);
        let bin = min(u32(code * f32(HISTOGRAM_BINS)), HISTOGRAM_BINS - 1u);
        atomicAdd(&local_bins[bin], 1u);
    }
    workgroupBarrier();

    let count = atomicLoad(&local_bins[local]);
    if count > 0u {
        atomicAdd(&histogram[local], count);
    }
}
