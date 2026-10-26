override EROSION_TILE: u32 = WORKGROUP_X + 2u * BOUNDARY_EROSION_COLUMNS;

@group(0) @binding(0) var boundary_store: texture_storage_2d<rg32float, write>;
@group(1) @binding(0) var<uniform> params: ColorParams;
@group(1) @binding(2) var boundary_table: texture_2d<f32>;

var<workgroup> erosion_tile: array<vec2<f32>, EROSION_TILE>;

fn wrapped_column(column: i32, width: u32) -> u32 {
    let columns = i32(width);
    return u32(((column % columns) + columns) % columns);
}

@compute @workgroup_size(WORKGROUP_X, WORKGROUP_Y)
fn erode_boundary(
    @builtin(global_invocation_id) id: vec3<u32>,
    @builtin(local_invocation_index) lane: u32,
    @builtin(workgroup_id) group: vec3<u32>,
) {
    let size = textureDimensions(boundary_store);
    let row = min(group.y, size.y - 1u);
    let first = i32(group.x * WORKGROUP_X) - i32(BOUNDARY_EROSION_COLUMNS);
    for (var index = lane; index < EROSION_TILE; index += WORKGROUP_X) {
        erosion_tile[index] = boundary_node(wrapped_column(first + i32(index), size.x), row);
    }
    workgroupBarrier();
    if id.x >= size.x || id.y >= size.y {
        return;
    }

    let cone_step = FULL_TURN / f32(size.x) * BOUNDARY_HUE_SLOPE * params.chroma_limits.xy;
    let centre = lane + BOUNDARY_EROSION_COLUMNS;
    var node = erosion_tile[centre];
    for (var offset = 1u; offset <= BOUNDARY_EROSION_COLUMNS; offset += 1u) {
        let cone = f32(offset) * cone_step;
        if all(cone >= node) {
            break;
        }
        let neighbours = min(erosion_tile[centre + offset], erosion_tile[centre - offset]);
        node = min(node, neighbours + cone);
    }
    textureStore(boundary_store, vec2<i32>(id.xy), vec4<f32>(node, 0.0, 0.0));
}
