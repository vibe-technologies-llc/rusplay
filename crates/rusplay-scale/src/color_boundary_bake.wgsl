@group(0) @binding(0) var boundary_store: texture_storage_2d<rg32float, write>;
@group(1) @binding(0) var<uniform> params: ColorParams;
@group(1) @binding(2) var boundary_table: texture_2d<f32>;

@compute @workgroup_size(WORKGROUP_X, WORKGROUP_Y)
fn bake_boundary(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(boundary_store);
    if id.x >= size.x || id.y >= size.y {
        return;
    }

    let lightness = params.chroma_limits.z * f32(id.y) / f32(size.y - 1u);
    let angle = FULL_TURN * f32(id.x) / f32(size.x);
    let hue = vec2<f32>(cos(angle), sin(angle));
    let peak = params.levels.x;
    let target_limit = chroma_boundary(lightness, hue, params.work_to_target, peak, params.chroma_limits.x);
    let source_limit = chroma_boundary(lightness, hue, params.work_to_source, peak, params.chroma_limits.y);
    textureStore(boundary_store, vec2<i32>(id.xy), vec4<f32>(target_limit, source_limit, 0.0, 0.0));
}
