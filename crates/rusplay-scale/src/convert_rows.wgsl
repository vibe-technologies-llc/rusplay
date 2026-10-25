const MAX_WORD_SAMPLES: u32 = 4u;
override MAX_CHROMA_TAPS: u32 = 32u;
override TILE_STRIDE: u32 = WORKGROUP_X * MAX_WORD_SAMPLES + MAX_CHROMA_TAPS;

@group(0) @binding(0) var source: texture_2d<f32>;
@group(1) @binding(0) var<storage, read_write> output: array<u32>;
@group(2) @binding(0) var<uniform> params: Params;
@group(2) @binding(1) var<storage, read> firsts: array<i32>;
@group(2) @binding(2) var<storage, read> weights: array<f32>;
@group(3) @binding(0) var chroma: texture_storage_2d<rg32float, write>;

var<workgroup> tile_luma: array<f32, TILE_STRIDE * WORKGROUP_Y>;
var<workgroup> tile_chroma: array<vec2<f32>, TILE_STRIDE * WORKGROUP_Y>;

@compute @workgroup_size(WORKGROUP_X, WORKGROUP_Y)
fn convert_rows(@builtin(global_invocation_id) id: vec3<u32>, @builtin(local_invocation_id) lid: vec3<u32>) {
    let per_word = samples_per_word();
    let chroma_per_word = per_word / 2u;
    let row = min(id.y, params.luma_size.y - 1u);
    let base = lid.y * TILE_STRIDE;

    let tile_first_pixel = i32((id.x - lid.x) * per_word);
    let tile_last_pixel = tile_first_pixel + i32(WORKGROUP_X * per_word) - 1;
    let first_chroma = min(u32(tile_first_pixel) / 2u, params.chroma_size.x - 1u);
    let last_chroma = min(u32(tile_last_pixel) / 2u, params.chroma_size.x - 1u);
    let low = min(firsts[first_chroma], tile_first_pixel);
    let high = max(firsts[last_chroma] + i32(params.horizontal_taps) - 1, tile_last_pixel);
    let count = min(u32(high - low) + 1u, TILE_STRIDE);

    let source_last = i32(params.luma_size.x) - 1;
    for (var entry = lid.x; entry < count; entry += WORKGROUP_X) {
        let x = clamp(low + i32(entry), 0, source_last);
        let ycc = encoded_ycc(textureLoad(source, vec2<i32>(x, i32(row)), 0));
        tile_luma[base + entry] = ycc.x;
        tile_chroma[base + entry] = ycc.yz;
    }
    workgroupBarrier();

    if id.x >= params.luma_stride || id.y >= params.luma_size.y {
        return;
    }

    var luma_word = 0u;
    for (var index = 0u; index < per_word; index++) {
        let x = id.x * per_word + index;
        if x < params.luma_size.x {
            let entry = base + u32(i32(x) - low);
            luma_word |= quantize(tile_luma[entry], params.luma_quant) << sample_bit_offset(index);
        }
    }
    output[params.luma_offset + id.y * params.luma_stride + id.x] = luma_word;

    for (var index = 0u; index < chroma_per_word; index++) {
        let x = id.x * chroma_per_word + index;
        if x >= params.chroma_size.x {
            continue;
        }

        let start = u32(firsts[x] - low);
        var sum = vec2<f32>(0.0);
        for (var tap = 0u; tap < params.horizontal_taps; tap++) {
            sum += weights[x * params.horizontal_taps + tap] * tile_chroma[base + start + tap];
        }
        textureStore(chroma, vec2<i32>(i32(x), i32(id.y)), vec4<f32>(sum, 0.0, 0.0));
    }
}
