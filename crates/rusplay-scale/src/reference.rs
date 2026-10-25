use crate::{
    Axis, TapTable,
    testing::{Image, quantize_f16},
};

pub(crate) fn resample_separable(source: &Image, table: &TapTable, axis: Axis) -> Image {
    let target_len = u32::try_from(table.firsts.len()).expect("table length fits u32");
    let (width, height) = match axis {
        Axis::Horizontal => (target_len, source.height),
        Axis::Vertical => (source.width, target_len),
    };

    Image::from_fn(width, height, |x, y| {
        let output_index = match axis {
            Axis::Horizontal => x,
            Axis::Vertical => y,
        } as usize;
        let taps = table.taps_per_pixel as usize;
        let first = i64::from(table.firsts[output_index]);
        let weights = &table.weights[output_index * taps..(output_index + 1) * taps];

        let mut sum = [0.0_f32; 4];
        for (offset, weight) in (0_i64..).zip(weights) {
            let along = first + offset;
            let texel = match axis {
                Axis::Horizontal => source.texel(along, i64::from(y)),
                Axis::Vertical => source.texel(i64::from(x), along),
            };
            for (total, value) in sum.iter_mut().zip(texel) {
                *total += weight * value;
            }
        }
        sum.map(quantize_f16)
    })
}
