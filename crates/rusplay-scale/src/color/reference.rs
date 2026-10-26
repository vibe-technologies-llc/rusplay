use std::{cell::RefCell, collections::HashMap, f64::consts::TAU, rc::Rc};

use rusplay_core::{HdrLevels, Matrix3, pq_from_nits, pq_to_nits};

use crate::color::{
    BOUNDARY_APPROACH_STEPS, BOUNDARY_COARSE_STEPS, BOUNDARY_HEADROOM, BOUNDARY_HUE_SLOPE,
    BOUNDARY_LOWER_TOLERANCE, BOUNDARY_RAMP_CENTRE, BOUNDARY_REACH, BOUNDARY_SUBSTEPS,
    BOUNDARY_TABLE_HUES, BOUNDARY_TABLE_LIGHTNESS, BOUNDARY_UPPER_TOLERANCE, BOUNDARY_WINDOW_START,
    BOUNDARY_WINDOW_STEPS, COMPRESSION_POWER, ColorPlan, HISTOGRAM_BINS, MappingPath, ToneCurve,
    ToneStage, WorkingSpace, boundary_erosion_columns,
};

const GAMUT_EPSILON: f64 = 1.0e-5;
const MIN_BOUND_DISTANCE: f64 = 1.0e-6;
const PQ_M1: f64 = 2610.0 / 16384.0;
const PQ_M2: f64 = 2523.0 / 4096.0 * 128.0;
const PQ_C1: f64 = 3424.0 / 4096.0;
const PQ_C2: f64 = 2413.0 / 4096.0 * 32.0;
const PQ_C3: f64 = 2392.0 / 4096.0 * 32.0;
const ACHROMATIC_CHROMA: f64 = 1.0e-6;
const MIN_COMPRESSION_LIMIT: f64 = 1.0001;
const MAX_NITS: f64 = HdrLevels::MAX_NITS as f64;
const MIN_AVERAGE_NITS: f64 = 1.0e-4;

fn signed(value: f64, curve: impl Fn(f64) -> f64) -> f64 {
    value.signum() * curve(value.abs())
}

pub(crate) fn xyz_to_lab(plan: &ColorPlan, xyz: [f64; 3]) -> [f64; 3] {
    plan.lab(xyz)
}

pub(crate) fn lab_to_xyz(plan: &ColorPlan, lab: [f64; 3]) -> [f64; 3] {
    let shaped = plan.lab_to_lms.apply(lab);
    let lms = match plan.space {
        WorkingSpace::Ictcp => shaped.map(|value| {
            signed(value, |code| pq_to_nits(code.min(1.0)) / MAX_NITS) / plan.pq_scale
        }),
        WorkingSpace::Oklab => shaped.map(|value| value * value * value),
    };

    plan.lms_to_xyz.apply(lms)
}

pub(crate) fn target_lab(plan: &ColorPlan, rgb: [f64; 3]) -> [f64; 3] {
    let target_to_work = plan.work_to_target.inverse().unwrap_or(Matrix3::IDENTITY);

    xyz_to_lab(plan, target_to_work.apply(rgb))
}

pub(crate) fn source_lab(plan: &ColorPlan, rgb: [f64; 3]) -> [f64; 3] {
    xyz_to_lab(
        plan,
        plan.source_to_work
            .apply(rgb.map(|value| value * plan.gain)),
    )
}

fn inside(rgb: [f64; 3], peak: f64) -> bool {
    let margin = GAMUT_EPSILON * peak;

    rgb.iter()
        .all(|value| (-margin..=peak + margin).contains(value))
}

fn at_chroma(plan: &ColorPlan, lightness: f64, hue: [f64; 2], chroma: f64) -> [f64; 3] {
    lab_to_xyz(plan, [lightness, hue[0] * chroma, hue[1] * chroma])
}

fn pq_decode_slope(code: f64) -> f64 {
    let clamped = code.clamp(0.0, 1.0);
    let power = clamped.powf(1.0 / PQ_M2);
    let numerator = power - PQ_C1;
    let denominator = PQ_C2 - PQ_C3 * power;
    if numerator <= 0.0 || code >= 1.0 {
        return 0.0;
    }
    let decoded = (numerator / denominator).powf(1.0 / PQ_M1);

    decoded * (PQ_C2 - PQ_C3 * PQ_C1) * power / (PQ_M1 * PQ_M2 * numerator * denominator * clamped)
}

#[derive(Debug, Clone, Copy)]
struct RaySample {
    value: [f64; 3],
    slope: [f64; 3],
}

struct Ray<'a> {
    plan: &'a ColorPlan,
    to_rgb: &'a Matrix3,
    origin: [f64; 3],
    direction: [f64; 3],
}

impl Ray<'_> {
    fn sample(&self, chroma: f64) -> RaySample {
        let plan = self.plan;
        let shaped = [0, 1, 2].map(|axis| self.origin[axis] + chroma * self.direction[axis]);
        let lms = shaped.map(|code| match plan.space {
            WorkingSpace::Ictcp => {
                signed(code, |code| pq_to_nits(code.min(1.0)) / MAX_NITS) / plan.pq_scale
            }
            WorkingSpace::Oklab => code * code * code,
        });
        let slope = [0, 1, 2].map(|axis| {
            let code = shaped[axis];
            let rate = match plan.space {
                WorkingSpace::Ictcp => pq_decode_slope(code.abs()) / plan.pq_scale,
                WorkingSpace::Oklab => 3.0 * code * code,
            };
            rate * self.direction[axis]
        });
        let to_rgb = |lms: [f64; 3]| self.to_rgb.apply(plan.lms_to_xyz.apply(lms));

        RaySample {
            value: to_rgb(lms),
            slope: to_rgb(slope),
        }
    }
}

struct Bounds {
    peak: f64,
    lower: [f64; 3],
    upper: [f64; 3],
}

impl Bounds {
    fn excess(&self, rgb: [f64; 3]) -> f64 {
        let worst = (0..3)
            .map(|channel| {
                let below = -rgb[channel] / self.lower[channel];
                let above = (rgb[channel] - self.peak) / self.upper[channel];
                below.max(above)
            })
            .fold(f64::NEG_INFINITY, f64::max);

        worst + BOUNDARY_RAMP_CENTRE
    }

    fn reach(&self, grey: RaySample) -> f64 {
        (0..3)
            .map(|channel| {
                let slope = grey.slope[channel];
                if slope < 0.0 {
                    grey.value[channel] / -slope
                } else if slope > 0.0 {
                    (self.peak - grey.value[channel]) / slope
                } else {
                    f64::INFINITY
                }
            })
            .fold(f64::INFINITY, f64::min)
    }
}

fn weight(level: f64) -> f64 {
    1.0 - level.min(1.0)
}

fn share(previous: f64, excess: f64, level: f64) -> f64 {
    if excess <= level {
        return weight(level);
    }

    let crossing = if previous >= level {
        0.0
    } else {
        (level - previous) / (excess - previous)
    };
    let (low, high) = (level.min(1.0), excess.min(1.0));
    let soft = (high - low) * (1.0 - f64::midpoint(low, high)) / (excess - level);

    crossing * weight(level) + (1.0 - crossing) * soft
}

fn hermite(near: RaySample, far: RaySample, width: f64, fraction: f64) -> [f64; 3] {
    let squared = fraction * fraction;
    let cubed = squared * fraction;
    let weights = [
        2.0 * cubed - 3.0 * squared + 1.0,
        (cubed - 2.0 * squared + fraction) * width,
        -2.0 * cubed + 3.0 * squared,
        (cubed - squared) * width,
    ];

    [0, 1, 2].map(|channel| {
        weights[0] * near.value[channel]
            + weights[1] * near.slope[channel]
            + weights[2] * far.value[channel]
            + weights[3] * far.slope[channel]
    })
}

fn hull_excess(bounds: &Bounds, near: RaySample, far: RaySample, width: f64) -> f64 {
    let leaving = [0, 1, 2].map(|channel| near.value[channel] + width * near.slope[channel] / 3.0);
    let arriving = [0, 1, 2].map(|channel| far.value[channel] - width * far.slope[channel] / 3.0);

    bounds
        .excess(leaving)
        .max(bounds.excess(arriving))
        .max(bounds.excess(far.value))
}

struct Breakpoints {
    approach_end: f64,
    approach_steps: u32,
    window_end: f64,
    window_steps: u32,
}

impl Breakpoints {
    fn at(&self, step: u32) -> f64 {
        if step <= self.approach_steps {
            self.approach_end * f64::from(step) / f64::from(self.approach_steps)
        } else {
            let into = f64::from(step - self.approach_steps) / f64::from(self.window_steps);
            self.approach_end + into * (self.window_end - self.approach_end)
        }
    }
}

fn soft_exit(ray: &Ray, bounds: &Bounds, grey: RaySample, breakpoints: &Breakpoints) -> f64 {
    let mut previous = bounds.excess(grey.value);
    let mut level = 0.0_f64;
    let mut near = grey;
    let mut start = 0.0;
    let mut total = 0.0;

    for step in 1..=breakpoints.approach_steps + breakpoints.window_steps {
        let end = breakpoints.at(step);
        let width = end - start;
        let far = ray.sample(end);
        if level <= 0.0 && hull_excess(bounds, near, far, width) <= 0.0 {
            total += width;
            previous = bounds.excess(far.value);
        } else {
            let substep = width / f64::from(BOUNDARY_SUBSTEPS);
            for index in 1..=BOUNDARY_SUBSTEPS {
                let rgb = if index == BOUNDARY_SUBSTEPS {
                    far.value
                } else {
                    hermite(
                        near,
                        far,
                        width,
                        f64::from(index) / f64::from(BOUNDARY_SUBSTEPS),
                    )
                };
                let excess = bounds.excess(rgb);
                total += substep * share(previous, excess, level);
                level = level.max(excess);
                previous = excess;
                if level >= 1.0 {
                    return total;
                }
            }
        }
        near = far;
        start = end;
    }
    total
}

pub(crate) fn chroma_boundary(
    plan: &ColorPlan,
    lightness: f64,
    hue: [f64; 2],
    to_rgb: &Matrix3,
    limit: f64,
) -> f64 {
    let peak = plan.target_peak;
    let ray = Ray {
        plan,
        to_rgb,
        origin: plan.lab_to_lms.apply([lightness, 0.0, 0.0]),
        direction: plan.lab_to_lms.apply([0.0, hue[0], hue[1]]),
    };
    let grey = ray.sample(0.0);
    if !inside(grey.value, peak) {
        return 0.0;
    }

    let floor = MIN_BOUND_DISTANCE * peak;
    let bounds = Bounds {
        peak,
        lower: grey
            .value
            .map(|value| BOUNDARY_LOWER_TOLERANCE * value.max(floor)),
        upper: grey
            .value
            .map(|value| BOUNDARY_UPPER_TOLERANCE * (peak - value).max(floor)),
    };
    let coarse_range = (BOUNDARY_REACH * bounds.reach(grey)).min(limit);
    let coarse = soft_exit(
        &ray,
        &bounds,
        grey,
        &Breakpoints {
            approach_end: coarse_range,
            approach_steps: BOUNDARY_COARSE_STEPS,
            window_end: coarse_range,
            window_steps: 0,
        },
    );

    soft_exit(
        &ray,
        &bounds,
        grey,
        &Breakpoints {
            approach_end: BOUNDARY_WINDOW_START * coarse,
            approach_steps: BOUNDARY_APPROACH_STEPS,
            window_end: BOUNDARY_HEADROOM * coarse,
            window_steps: BOUNDARY_WINDOW_STEPS,
        },
    )
}

#[derive(Default)]
struct BoundaryNodes {
    exact: HashMap<(u32, u32), [f64; 2]>,
    eroded: HashMap<(u32, u32), [f64; 2]>,
}

thread_local! {
    static BOUNDARY_TABLES: RefCell<HashMap<Vec<u8>, Rc<RefCell<BoundaryNodes>>>> =
        RefCell::new(HashMap::new());
}

fn exact_node(plan: &ColorPlan, column: u32, row: u32) -> [f64; 2] {
    let lightness =
        plan.boundary_lightness * f64::from(row) / f64::from(BOUNDARY_TABLE_LIGHTNESS - 1);
    let angle = TAU * f64::from(column) / f64::from(BOUNDARY_TABLE_HUES);
    let hue = [angle.cos(), angle.sin()];

    [
        chroma_boundary(
            plan,
            lightness,
            hue,
            &plan.work_to_target,
            plan.target_chroma_limit,
        ),
        chroma_boundary(
            plan,
            lightness,
            hue,
            &plan.work_to_source,
            plan.source_chroma_limit,
        ),
    ]
}

impl BoundaryNodes {
    fn exact(&mut self, plan: &ColorPlan, column: u32, row: u32) -> [f64; 2] {
        *self
            .exact
            .entry((column, row))
            .or_insert_with(|| exact_node(plan, column, row))
    }

    fn eroded(&mut self, plan: &ColorPlan, column: u32, row: u32) -> [f64; 2] {
        if let Some(node) = self.eroded.get(&(column, row)) {
            return *node;
        }

        let reach = boundary_erosion_columns();
        let step = TAU / f64::from(BOUNDARY_TABLE_HUES) * BOUNDARY_HUE_SLOPE;
        let limits = [plan.target_chroma_limit, plan.source_chroma_limit];
        let mut node = self.exact(plan, column, row);
        for offset in 1..=reach {
            let cone = step * f64::from(offset);
            if (0..2).all(|limit| cone * limits[limit] >= node[limit]) {
                break;
            }
            for neighbour in [
                (column + offset) % BOUNDARY_TABLE_HUES,
                (column + BOUNDARY_TABLE_HUES - offset) % BOUNDARY_TABLE_HUES,
            ] {
                let exact = self.exact(plan, neighbour, row);
                node = [0, 1].map(|limit| node[limit].min(exact[limit] + cone * limits[limit]));
            }
        }
        self.eroded.insert((column, row), node);
        node
    }
}

fn boundary_nodes(plan: &ColorPlan) -> Rc<RefCell<BoundaryNodes>> {
    BOUNDARY_TABLES.with_borrow_mut(|tables| tables.entry(plan.params_bytes()).or_default().clone())
}

pub(crate) fn boundary_limits(plan: &ColorPlan, lightness: f64, angle: f64) -> [f64; 2] {
    let last_row = BOUNDARY_TABLE_LIGHTNESS - 1;
    let rows = (lightness / plan.boundary_lightness).clamp(0.0, 1.0) * f64::from(last_row);
    let columns = (angle / TAU).rem_euclid(1.0) * f64::from(BOUNDARY_TABLE_HUES);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let (row, column) = (
        (rows as u32).min(last_row - 1),
        (columns as u32).min(BOUNDARY_TABLE_HUES - 1),
    );
    let next = (column + 1) % BOUNDARY_TABLE_HUES;
    let (across, up) = (columns - f64::from(column), rows - f64::from(row));
    let mix = |from: [f64; 2], to: [f64; 2], amount: f64| {
        [0, 1].map(|limit| from[limit] + amount * (to[limit] - from[limit]))
    };
    let nodes = boundary_nodes(plan);
    let mut nodes = nodes.borrow_mut();
    let low = mix(
        nodes.eroded(plan, column, row),
        nodes.eroded(plan, next, row),
        across,
    );
    let high = mix(
        nodes.eroded(plan, column, row + 1),
        nodes.eroded(plan, next, row + 1),
        across,
    );

    mix(low, high, up)
}

pub(crate) fn compress_chroma(chroma: f64, target_limit: f64, source_limit: f64, knee: f64) -> f64 {
    if target_limit <= 0.0 {
        return 0.0;
    }

    let distance = chroma / target_limit;
    let limit = source_limit / target_limit;
    if limit <= MIN_COMPRESSION_LIMIT || distance <= knee {
        return distance.min(1.0) * target_limit;
    }

    let power = COMPRESSION_POWER;
    let scale =
        (limit - knee) / (((1.0 - knee) / (limit - knee)).powf(-power) - 1.0).powf(1.0 / power);
    let excess = (distance - knee) / scale;
    let compressed = knee + (distance - knee) / (1.0 + excess.powf(power)).powf(1.0 / power);

    compressed.min(1.0) * target_limit
}

fn clamp(rgb: [f64; 3], peak: f64) -> [f64; 3] {
    rgb.map(|value| value.clamp(0.0, peak))
}

fn perceptual(plan: &ColorPlan, work: [f64; 3]) -> [f64; 3] {
    let peak = plan.target_peak;
    let [lightness, a, b] = xyz_to_lab(plan, work);
    let chroma = a.hypot(b);
    if chroma < ACHROMATIC_CHROMA {
        return clamp(plan.work_to_target.apply(work), peak);
    }

    let hue = [a / chroma, b / chroma];
    let [target_limit, source_limit] = boundary_limits(plan, lightness, b.atan2(a));
    if chroma <= plan.knee * target_limit {
        return clamp(plan.work_to_target.apply(work), peak);
    }
    let mapped = compress_chroma(chroma, target_limit, source_limit, plan.knee);

    clamp(
        plan.work_to_target
            .apply(at_chroma(plan, lightness, hue, mapped)),
        peak,
    )
}

fn desaturate(plan: &ColorPlan, rgb: [f64; 3]) -> [f64; 3] {
    let peak = plan.target_peak;
    let grey = plan
        .target_luma
        .iter()
        .zip(rgb)
        .map(|(weight, value)| weight * value)
        .sum::<f64>()
        .clamp(0.0, peak);
    let amount = rgb.iter().fold(1.0_f64, |amount, &value| {
        if value < 0.0 {
            amount.min(grey / (grey - value))
        } else if value > peak {
            amount.min((peak - grey) / (value - grey))
        } else {
            amount
        }
    });

    clamp(rgb.map(|value| grey + amount * (value - grey)), peak)
}

fn highlight(plan: &ColorPlan, rgb: [f64; 3]) -> [f64; 3] {
    let peak = plan.target_peak;
    let clipped = clamp(rgb, peak);

    if inside(rgb, peak) {
        clipped
    } else {
        clipped.map(|value| peak - value)
    }
}

pub(crate) fn tone_map(plan: &ColorPlan, curve: &ToneCurve, rgb: [f64; 3]) -> [f64; 3] {
    let xyz = plan
        .source_to_work
        .apply(rgb)
        .map(|value| value * plan.source_white_nits);
    let lms = plan.tone_xyz_to_lms.apply(xyz);
    let itp = plan
        .tone_lms_to_itp
        .apply(lms.map(|value| signed(value, pq_from_nits)));

    let intensity = itp[0];
    let mapped = curve.map_pq(intensity.max(0.0));
    let ratio = if intensity > 0.0 && mapped > 0.0 {
        (mapped / intensity).min(intensity / mapped)
    } else {
        1.0
    };

    let toned = [mapped, itp[1] * ratio, itp[2] * ratio];
    let toned_lms = plan
        .tone_itp_to_lms
        .apply(toned)
        .map(|value| signed(value, |code| pq_to_nits(code.min(1.0))));
    plan.work_to_source
        .apply(plan.tone_lms_to_xyz.apply(toned_lms))
        .map(|value| value / plan.target_white_nits)
}

pub(crate) fn tone_intensity(plan: &ColorPlan, rgb: [f64; 3], white_nits: f64) -> [f64; 3] {
    let xyz = plan
        .source_to_work
        .apply(rgb)
        .map(|value| value * white_nits);
    let lms = plan.tone_xyz_to_lms.apply(xyz);

    plan.tone_lms_to_itp
        .apply(lms.map(|value| signed(value, pq_from_nits)))
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) struct PeakState {
    pub smoothed_peak_pq: f64,
    pub previous_average_pq: f64,
    pub frames: u32,
}

pub(crate) fn histogram(plan: &ColorPlan, pixels: &[[f32; 4]]) -> Vec<u32> {
    let mut bins = vec![0_u32; HISTOGRAM_BINS as usize];
    for pixel in pixels {
        let brightest = pixel[..3].iter().copied().fold(0.0_f32, f32::max);
        let code = pq_from_nits(f64::from(brightest) * plan.source_white_nits);

        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let bin = ((code * f64::from(HISTOGRAM_BINS)) as u32).min(HISTOGRAM_BINS - 1);
        bins[bin as usize] += 1;
    }
    bins
}

pub(crate) fn detect_peak(
    stage: &ToneStage,
    previous: PeakState,
    bins: &[u32],
) -> (PeakState, ToneCurve) {
    let dynamic = stage.dynamic_peak.unwrap_or_default();
    let total: u32 = bins.iter().sum();
    let wanted = dynamic.percentile / 100.0 * f64::from(total);
    let bin_count = f64::from(HISTOGRAM_BINS);

    let mut cumulative = 0;
    let peak_bin = bins
        .iter()
        .position(|count| {
            cumulative += count;
            f64::from(cumulative) >= wanted
        })
        .unwrap_or(bins.len() - 1);
    let weighted: f64 = (0_u32..)
        .zip(bins)
        .map(|(bin, count)| f64::from(*count) * (f64::from(bin) + 0.5))
        .sum();

    #[allow(clippy::cast_precision_loss)]
    let frame_peak = (peak_bin + 1) as f64 / bin_count;
    let average = weighted / f64::from(total) / bin_count;
    let level = |code: f64| pq_to_nits(code).max(MIN_AVERAGE_NITS);
    let change = (10.0 * (level(average) / level(previous.previous_average_pq)).log10()).abs();
    let smoothed = if previous.frames > 0 && change <= dynamic.scene_threshold_db {
        previous.smoothed_peak_pq
            + (frame_peak - previous.smoothed_peak_pq) / f64::from(dynamic.smoothing_frames)
    } else {
        frame_peak
    };

    let static_peak = stage.curve.source_peak_nits;
    let target_peak = stage.curve.target_peak_nits;
    let curve = ToneCurve {
        source_peak_nits: pq_to_nits(smoothed).min(static_peak).max(target_peak),
        ..stage.curve
    };

    (
        PeakState {
            smoothed_peak_pq: smoothed,
            previous_average_pq: average,
            frames: previous.frames + 1,
        },
        curve,
    )
}

pub(crate) fn map(plan: &ColorPlan, rgb: [f64; 3]) -> [f64; 3] {
    map_with_curve(plan, plan.tone.map(|stage| stage.curve), rgb)
}

pub(crate) fn map_with_curve(
    plan: &ColorPlan,
    curve: Option<ToneCurve>,
    rgb: [f64; 3],
) -> [f64; 3] {
    let source = match curve {
        Some(curve) => tone_map(plan, &curve, rgb),
        None => rgb.map(|value| value * plan.gain),
    };
    let direct = plan.source_to_target.apply(source);

    match plan.path {
        MappingPath::Matrix => direct,
        MappingPath::Perceptual => perceptual(plan, plan.source_to_work.apply(source)),
        MappingPath::Clip => clamp(direct, plan.target_peak),
        MappingPath::Desaturate => desaturate(plan, direct),
        MappingPath::Highlight => highlight(plan, direct),
    }
}
