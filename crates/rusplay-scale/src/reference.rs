use rusplay_core::{HdrLevels, TransferFunction};

use crate::{
    ConvertSettings, FrameLayout, InputLight, Kernel, LineariseSettings, OutputLight, PlaneContent,
    PlaneLayout, Quantization, RgbConvertSettings, ScaleSettings, TapTable,
    polar::PolarGeometry,
    scaler::Axis,
    shaping::Shaping,
    testing::{Image, quantize_f16},
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

pub(crate) struct PolarFootprint {
    pub taps: Vec<(i64, i64, f64)>,
    pub anchor: (i64, i64),
}

pub(crate) fn polar_footprint(
    geometry: &PolarGeometry,
    kernel: Kernel,
    x: u32,
    y: u32,
) -> PolarFootprint {
    let radius = f64::from(geometry.radius);
    let stretch = (f64::from(geometry.stretch.0), f64::from(geometry.stretch.1));

    #[allow(clippy::cast_precision_loss)]
    let center = (
        f64::from((x as f32 + 0.5) * geometry.ratio.0 - 0.5 + geometry.phase.0),
        f64::from((y as f32 + 0.5) * geometry.ratio.1 - 0.5 + geometry.phase.1),
    );

    #[allow(clippy::cast_possible_truncation)]
    let span = |middle: f64, stretch: f64| {
        ((middle - radius * stretch).floor() as i64 + 1)
            ..=((middle + radius * stretch).ceil() as i64 - 1)
    };

    #[allow(clippy::cast_possible_truncation)]
    let anchor = (center.0.floor() as i64, center.1.floor() as i64);

    let taps = span(center.1, stretch.1)
        .flat_map(|row| span(center.0, stretch.0).map(move |column| (column, row)))
        .filter_map(|(column, row)| {
            #[allow(clippy::cast_precision_loss)]
            let distance =
                ((column as f64 - center.0) / stretch.0).hypot((row as f64 - center.1) / stretch.1);
            (distance < radius).then(|| (column, row, kernel.weight(distance)))
        })
        .collect();

    PolarFootprint { taps, anchor }
}

pub(crate) fn resample_polar(source: &Image, settings: &ScaleSettings) -> Image {
    let geometry = PolarGeometry::new(settings);
    let kernel = Kernel::new(settings.filter);
    let shaping = settings.polar_shaping();

    Image::from_fn(geometry.target.0, geometry.target.1, |x, y| {
        let footprint = polar_footprint(&geometry, kernel, x, y);

        let mut sum = [0.0_f64; 4];
        let mut total = 0.0_f64;
        let mut nearest_low = [f32::MAX; 4];
        let mut nearest_high = [f32::MIN; 4];
        for &(column, row, weight) in &footprint.taps {
            let texel = shaping.input(source.texel(column, row));
            for (accumulated, value) in sum.iter_mut().zip(texel) {
                *accumulated += weight * f64::from(value);
            }
            total += weight;

            if (0..=1).contains(&(column - footprint.anchor.0))
                && (0..=1).contains(&(row - footprint.anchor.1))
            {
                for channel in 0..4 {
                    nearest_low[channel] = nearest_low[channel].min(texel[channel]);
                    nearest_high[channel] = nearest_high[channel].max(texel[channel]);
                }
            }
        }

        #[allow(clippy::cast_possible_truncation)]
        let color = sum.map(|value| (value / total) as f32);
        shaping.output(shaping.antiring(color, nearest_low, nearest_high))
    })
}

pub(crate) type SamplePlanes = Vec<Vec<Vec<u32>>>;

const PQ_M1: f64 = 2610.0 / 16384.0;
const PQ_M2: f64 = 2523.0 / 4096.0 * 128.0;
const PQ_C1: f64 = 3424.0 / 4096.0;
const PQ_C2: f64 = 2413.0 / 4096.0 * 32.0;
const PQ_C3: f64 = 2392.0 / 4096.0 * 32.0;
const HLG_A: f64 = 0.178_832_77;
const HLG_B: f64 = 1.0 - 4.0 * HLG_A;

fn pq_oetf(display: f64) -> f64 {
    let power = display.powf(PQ_M1);

    ((PQ_C1 + PQ_C2 * power) / (1.0 + PQ_C3 * power)).powf(PQ_M2)
}

fn hlg_oetf(scene: f64) -> f64 {
    if scene <= 1.0 / 12.0 {
        (3.0 * scene).sqrt()
    } else {
        HLG_A * (12.0 * scene - HLG_B).ln() + (0.5 - HLG_A * (4.0 * HLG_A).ln())
    }
}

fn hlg_scene_light(settings: &ConvertSettings, display: [f64; 3]) -> [f64; 3] {
    let luminance = dot(settings.matrix.rows()[0], display);
    if luminance <= 0.0 {
        return [0.0; 3];
    }

    let gain = luminance.powf(settings.hlg_exponent());
    display.map(|component| (component * gain).clamp(0.0, 1.0))
}

fn hlg_inverse_oetf(code: f64) -> f64 {
    if code <= 0.5 {
        code * code / 3.0
    } else {
        (((code - (0.5 - HLG_A * (4.0 * HLG_A).ln())) / HLG_A).exp() + HLG_B) / 12.0
    }
}

fn srgb_eotf(code: f64) -> f64 {
    if code <= 0.040_45 {
        code / 12.92
    } else {
        ((code + 0.055) / 1.055).powf(2.4)
    }
}

fn bt709_inverse_oetf(code: f64) -> f64 {
    if code < 0.081 {
        code / 4.5
    } else {
        ((code + 0.099) / 1.099).powf(1.0 / 0.45)
    }
}

pub(crate) fn linearise_texel(settings: &LineariseSettings, rgb: [f64; 3]) -> [f64; 3] {
    let code = rgb.map(|value| value.clamp(0.0, 1.0));
    let scale = settings.hdr_scale();

    match (settings.output, settings.transfer) {
        (OutputLight::Encoded, _) | (OutputLight::Linear, TransferFunction::Linear) => rgb,
        (OutputLight::Linear, TransferFunction::Srgb) => code.map(srgb_eotf),
        (OutputLight::Linear, TransferFunction::Bt709) => code.map(bt709_inverse_oetf),
        (OutputLight::Linear, TransferFunction::Gamma22) => code.map(|value| value.powf(2.2)),
        (OutputLight::Linear, TransferFunction::Pq) => code
            .map(|value| rusplay_core::pq_to_nits(value) / f64::from(HdrLevels::MAX_NITS) * scale),
        (OutputLight::Linear, TransferFunction::Hlg) => {
            let scene = code.map(hlg_inverse_oetf);
            let luminance = dot(settings.matrix.rows()[0], scene);
            if luminance <= 0.0 {
                return [0.0; 3];
            }
            let gain = luminance.powf(settings.hlg_exponent());
            scene.map(|value| value * gain * scale)
        }
    }
}

pub(crate) fn linearise(source: &Image, settings: &LineariseSettings) -> Image {
    let lanes = settings.channels.order.lanes().map(|lane| lane as usize);

    Image::from_fn(source.width, source.height, |x, y| {
        let texel = source.texel(i64::from(x), i64::from(y));
        let rgb = [0, 1, 2].map(|channel| f64::from(texel[lanes[channel]]));
        let alpha = if settings.channels.opaque {
            1.0
        } else {
            texel[lanes[3]]
        };
        let [red, green, blue] = linearise_texel(settings, rgb);

        #[allow(clippy::cast_possible_truncation)]
        let narrowed = [red as f32, green as f32, blue as f32, alpha];
        narrowed
    })
}

pub(crate) fn encode_linear(settings: &ConvertSettings, linear: [f64; 3]) -> [f64; 3] {
    #[allow(clippy::cast_possible_truncation)]
    let texel = [linear[0] as f32, linear[1] as f32, linear[2] as f32, 1.0];

    encode_texel(settings, texel)
}

fn encode_texel(settings: &ConvertSettings, texel: [f32; 4]) -> [f64; 3] {
    let linear = [0, 1, 2].map(|channel| f64::from(texel[channel]));
    let display = linear.map(|value| (value * settings.hdr_scale()).clamp(0.0, 1.0));
    let hlg_scene = hlg_scene_light(settings, display);

    std::array::from_fn(|channel| {
        let linear = linear[channel].clamp(0.0, 1.0);
        match (settings.input, settings.transfer) {
            (InputLight::Linear, TransferFunction::Srgb) if linear > 0.003_130_8 => {
                1.055 * linear.powf(1.0 / 2.4) - 0.055
            }
            (InputLight::Linear, TransferFunction::Srgb) => 12.92 * linear,
            (InputLight::Linear, TransferFunction::Bt709) if linear >= 0.018 => {
                1.099 * linear.powf(0.45) - 0.099
            }
            (InputLight::Linear, TransferFunction::Bt709) => 4.5 * linear,
            (InputLight::Linear, TransferFunction::Pq) => pq_oetf(display[channel]),
            (InputLight::Linear, TransferFunction::Hlg) => hlg_oetf(hlg_scene[channel]),
            (InputLight::Linear, TransferFunction::Gamma22) => linear.powf(1.0 / 2.2),
            (InputLight::Linear, TransferFunction::Linear) | (InputLight::Encoded, _) => linear,
        }
    })
}

fn dot(row: [f64; 3], rgb: [f64; 3]) -> f64 {
    row.iter().zip(rgb).map(|(a, b)| a * b).sum()
}

fn quantize(settings: &ConvertSettings, value: f64, (scale, offset): (f64, f64)) -> u32 {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let code = (value * scale + offset)
        .round()
        .clamp(0.0, f64::from(settings.quantization().max_code)) as u32;
    code
}

fn filter_taps(table: &TapTable, index: u32, sample: &dyn Fn(i64) -> [f64; 2]) -> [f64; 2] {
    let taps = table.taps_per_pixel as usize;
    let start = index as usize * taps;
    (0_i64..)
        .zip(&table.weights[start..start + taps])
        .fold([0.0; 2], |sum, (offset, weight)| {
            let value = sample(i64::from(table.firsts[index as usize]) + offset);
            [
                sum[0] + f64::from(*weight) * value[0],
                sum[1] + f64::from(*weight) * value[1],
            ]
        })
}

fn polar_chroma_plane(
    settings: &ConvertSettings,
    chroma_at: &dyn Fn(i64, i64) -> [f64; 2],
) -> Vec<Vec<[f64; 2]>> {
    let geometry = settings.polar_chroma_geometry();
    let kernel = Kernel::new(settings.chroma_filter);
    let (chroma_width, chroma_height) = settings.chroma_size();

    (0..chroma_height)
        .map(|y| {
            (0..chroma_width)
                .map(|x| {
                    let footprint = polar_footprint(&geometry, kernel, x, y);
                    let total: f64 = footprint.taps.iter().map(|tap| tap.2).sum();

                    footprint
                        .taps
                        .iter()
                        .fold([0.0; 2], |sum, &(column, row, weight)| {
                            let value = chroma_at(column, row);
                            [
                                sum[0] + weight * value[0] / total,
                                sum[1] + weight * value[1] / total,
                            ]
                        })
                })
                .collect()
        })
        .collect()
}

fn separable_chroma_plane(
    settings: &ConvertSettings,
    height: u32,
    chroma_at: &dyn Fn(i64, i64) -> [f64; 2],
) -> Vec<Vec<[f64; 2]>> {
    let (horizontal, vertical) = settings.chroma_tables();
    let (chroma_width, chroma_height) = settings.chroma_size();

    let rows_filtered: Vec<Vec<[f64; 2]>> = (0..height)
        .map(|y| {
            (0..chroma_width)
                .map(|x| filter_taps(&horizontal, x, &|column| chroma_at(column, i64::from(y))))
                .collect()
        })
        .collect();
    let last_row = i64::from(height) - 1;

    (0..chroma_height)
        .map(|y| {
            (0..chroma_width)
                .map(|x| {
                    filter_taps(&vertical, y, &|row| {
                        rows_filtered[usize::try_from(row.clamp(0, last_row)).unwrap_or_default()]
                            [x as usize]
                    })
                })
                .collect()
        })
        .collect()
}

pub(crate) fn convert(source: &Image, settings: &ConvertSettings) -> SamplePlanes {
    let rows = settings.matrix.rows();
    let quantization = settings.quantization();

    let luma: Vec<Vec<u32>> = (0..source.height)
        .map(|y| {
            (0..source.width)
                .map(|x| {
                    let rgb = encode_texel(settings, source.texel(i64::from(x), i64::from(y)));
                    quantize(settings, dot(rows[0], rgb), quantization.luma)
                })
                .collect()
        })
        .collect();
    let chroma_at = |x: i64, y: i64| {
        let rgb = encode_texel(settings, source.texel(x, y));
        [dot(rows[1], rgb), dot(rows[2], rgb)]
    };
    let filtered = if settings.polar_chroma() {
        polar_chroma_plane(settings, &chroma_at)
    } else {
        separable_chroma_plane(settings, source.height, &chroma_at)
    };
    let chroma: Vec<Vec<[u32; 2]>> = filtered
        .iter()
        .map(|row| {
            row.iter()
                .map(|pair| pair.map(|value| quantize(settings, value, quantization.chroma)))
                .collect()
        })
        .collect();

    let component = |index: usize| -> Vec<Vec<u32>> {
        chroma
            .iter()
            .map(|row| row.iter().map(|pair| pair[index]).collect())
            .collect()
    };
    match settings.layout {
        PlaneLayout::Planar | PlaneLayout::PlanarHigh => vec![luma, component(0), component(1)],
        PlaneLayout::SemiPlanar => vec![
            luma,
            chroma
                .iter()
                .map(|row| row.iter().flatten().copied().collect())
                .collect(),
        ],
    }
}

pub(crate) fn unpack(bytes: &[u8], layout: &FrameLayout) -> SamplePlanes {
    let sample_bytes = layout.bytes_per_sample as usize;
    let shift = layout.sample_shift();

    layout
        .planes
        .iter()
        .map(|plane| {
            let samples = (plane.width * plane.content.samples_per_pixel()) as usize;
            (0..plane.height as usize)
                .map(|row| {
                    let start = usize::try_from(plane.offset).unwrap_or_default()
                        + row * plane.stride as usize;
                    bytes[start..start + samples * sample_bytes]
                        .chunks_exact(sample_bytes)
                        .map(|sample| {
                            let raw = sample
                                .iter()
                                .rev()
                                .fold(0_u32, |value, byte| value << 8 | u32::from(*byte));
                            raw >> shift
                        })
                        .collect()
                })
                .collect()
        })
        .collect()
}

pub(crate) fn unpack_rgb(bytes: &[u8], settings: &RgbConvertSettings) -> Image {
    let layout = settings.frame_layout().unwrap();
    let planes = unpack(bytes, &layout);
    let quantization = Quantization::new(settings.format.depth, settings.range);
    let rows = settings.matrix.inverse_rows();
    let (horizontal, vertical) = settings.chroma_tables();
    let (chroma_width, chroma_height) = settings.chroma_size();
    let interleaved = layout.planes[1].content == PlaneContent::CbCr;
    let dequantize = |code: u32, (scale, offset): (f64, f64)| (f64::from(code) - offset) / scale;
    let chroma_at = |x: i64, y: usize| -> [f64; 2] {
        let x = usize::try_from(x.clamp(0, i64::from(chroma_width) - 1)).unwrap_or_default();
        let (cb, cr) = if interleaved {
            (planes[1][y][2 * x], planes[1][y][2 * x + 1])
        } else {
            (planes[1][y][x], planes[2][y][x])
        };
        [
            dequantize(cb, quantization.chroma),
            dequantize(cr, quantization.chroma),
        ]
    };
    let weighted = |sum: [f64; 2], weight: f32, value: [f64; 2]| {
        [
            sum[0] + f64::from(weight) * value[0],
            sum[1] + f64::from(weight) * value[1],
        ]
    };

    let rows_filtered: Vec<Vec<[f64; 2]>> = (0..chroma_height as usize)
        .map(|y| {
            (0..settings.resolution.width)
                .map(|x| {
                    taps(&horizontal, x).fold([0.0; 2], |sum, (column, weight)| {
                        weighted(sum, weight, chroma_at(column, y))
                    })
                })
                .collect()
        })
        .collect();
    let last_row = i64::from(chroma_height) - 1;
    let decode = |encoded: f64| {
        let x = encoded.clamp(0.0, 1.0);
        match (settings.output, settings.transfer) {
            (OutputLight::Linear, TransferFunction::Srgb) if x <= 0.040_45 => x / 12.92,
            (OutputLight::Linear, TransferFunction::Srgb) => ((x + 0.055) / 1.055).powf(2.4),
            (OutputLight::Linear, TransferFunction::Bt709) if x < 0.081 => x / 4.5,
            (OutputLight::Linear, TransferFunction::Bt709) => {
                ((x + 0.099) / 1.099).powf(1.0 / 0.45)
            }
            (OutputLight::Linear, TransferFunction::Gamma22) => x.powf(2.2),
            _ => x,
        }
    };

    Image::from_fn(
        settings.resolution.width,
        settings.resolution.height,
        |x, y| {
            let chroma = taps(&vertical, y).fold([0.0; 2], |sum, (row, weight)| {
                let row = usize::try_from(row.clamp(0, last_row)).unwrap_or_default();
                weighted(sum, weight, rows_filtered[row][x as usize])
            });
            let ycbcr = [
                dequantize(planes[0][y as usize][x as usize], quantization.luma),
                chroma[0],
                chroma[1],
            ];
            let channel = |row: [f64; 3]| {
                let encoded = row.iter().zip(ycbcr).map(|(a, b)| a * b).sum::<f64>();
                #[allow(clippy::cast_possible_truncation)]
                let linear = decode(encoded) as f32;
                quantize_f16(linear)
            };

            [channel(rows[0]), channel(rows[1]), channel(rows[2]), 1.0]
        },
    )
}

fn taps(table: &TapTable, index: u32) -> impl Iterator<Item = (i64, f32)> + '_ {
    let count = table.taps_per_pixel as usize;
    let start = index as usize * count;
    let first = i64::from(table.firsts[index as usize]);

    (first..).zip(table.weights[start..start + count].iter().copied())
}
