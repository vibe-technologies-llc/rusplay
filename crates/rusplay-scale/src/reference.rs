use crate::{
    Kernel, ScaleSettings, TapTable, polar::PolarGeometry, scaler::Axis, shaping::Shaping,
    testing::Image,
};

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

pub(crate) fn resample_polar(source: &Image, settings: &ScaleSettings) -> Image {
    let geometry = PolarGeometry::new(settings);
    let kernel = Kernel::new(settings.filter);
    let shaping = settings.polar_shaping();
    let radius = f64::from(geometry.radius);
    let stretch = (f64::from(geometry.stretch.0), f64::from(geometry.stretch.1));

    Image::from_fn(geometry.target.0, geometry.target.1, |x, y| {
        #[allow(clippy::cast_precision_loss)]
        let center = (
            f64::from((x as f32 + 0.5) * geometry.ratio.0 - 0.5),
            f64::from((y as f32 + 0.5) * geometry.ratio.1 - 0.5),
        );

        #[allow(clippy::cast_possible_truncation)]
        let span = |middle: f64, stretch: f64| {
            ((middle - radius * stretch).floor() as i64 + 1)
                ..=((middle + radius * stretch).ceil() as i64 - 1)
        };

        #[allow(clippy::cast_possible_truncation)]
        let anchor = (center.0.floor() as i64, center.1.floor() as i64);

        let mut sum = [0.0_f64; 4];
        let mut total = 0.0_f64;
        let mut nearest_low = [f32::MAX; 4];
        let mut nearest_high = [f32::MIN; 4];
        for row in span(center.1, stretch.1) {
            for column in span(center.0, stretch.0) {
                #[allow(clippy::cast_precision_loss)]
                let distance = ((column as f64 - center.0) / stretch.0)
                    .hypot((row as f64 - center.1) / stretch.1);
                if distance >= radius {
                    continue;
                }

                let weight = kernel.weight(distance);
                let texel = shaping.input(source.texel(column, row));
                for (accumulated, value) in sum.iter_mut().zip(texel) {
                    *accumulated += weight * f64::from(value);
                }
                total += weight;

                if (0..=1).contains(&(column - anchor.0)) && (0..=1).contains(&(row - anchor.1)) {
                    for channel in 0..4 {
                        nearest_low[channel] = nearest_low[channel].min(texel[channel]);
                        nearest_high[channel] = nearest_high[channel].max(texel[channel]);
                    }
                }
            }
        }

        #[allow(clippy::cast_possible_truncation)]
        let color = sum.map(|value| (value / total) as f32);
        shaping.output(shaping.antiring(color, nearest_low, nearest_high))
    })
}
