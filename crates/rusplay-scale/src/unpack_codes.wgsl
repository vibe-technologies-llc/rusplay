fn dequantize(code: u32, quant: vec2<f32>) -> f32 {
    return (f32(code) - quant.y) / quant.x;
}

fn luma_at(x: u32, y: u32) -> f32 {
    return dequantize(luma_code(x, y), unpacking.luma_quant);
}

fn chroma_at(x: u32, y: u32) -> vec2<f32> {
    let codes = chroma_codes(x, y);
    return vec2<f32>(dequantize(codes.x, unpacking.chroma_quant), dequantize(codes.y, unpacking.chroma_quant));
}
