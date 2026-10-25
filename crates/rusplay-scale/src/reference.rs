use crate::{TapTable, scaler::Axis, shaping::Shaping, testing::Image};

pub(crate) fn resample_separable(
    source: &Image,
    table: &TapTable,
    axis: Axis,
    shaping: &Shaping,
) -> Image {
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
        let anchor = i64::from(table.anchors[output_index]);
        let weights = &table.weights[output_index * taps..(output_index + 1) * taps];

        let mut sum = [0.0_f32; 4];
        let mut nearest_low = [f32::MAX; 4];
        let mut nearest_high = [f32::MIN; 4];
        for (offset, weight) in (0_i64..).zip(weights) {
            let position = first + offset;
            let texel = shaping.input(match axis {
                Axis::Horizontal => source.texel(position, i64::from(y)),
                Axis::Vertical => source.texel(i64::from(x), position),
            });
            for (total, value) in sum.iter_mut().zip(texel) {
                *total += weight * value;
            }
            if (0..=1).contains(&(position - anchor)) {
                for channel in 0..4 {
                    nearest_low[channel] = nearest_low[channel].min(texel[channel]);
                    nearest_high[channel] = nearest_high[channel].max(texel[channel]);
                }
            }
        }

        shaping.output(shaping.antiring(sum, nearest_low, nearest_high))
    })
}
